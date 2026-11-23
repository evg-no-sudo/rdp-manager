#include "rdp_bridge.h"
#include <freerdp/freerdp.h>
#include <freerdp/settings.h>
#include <freerdp/input.h>
#include <freerdp/update.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/gdi/gfx.h>
#include <freerdp/client/rdpgfx.h>
#include <freerdp/client/disp.h>
#include <freerdp/client/channels.h>
#include <freerdp/client/cmdline.h>
#include <freerdp/event.h>
#include <freerdp/codec/color.h>
#include <freerdp/crypto/certificate.h>
#include <freerdp/utils/helpers.h>
#include <winpr/synch.h>
#include <winpr/sysinfo.h>
#include <winpr/ssl.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

struct RdpBridge {
    freerdp* instance;
    HANDLE input_event;
    atomic_uint_fast64_t input_wakes, input_scans, input_unicode;
    void* opaque;
    RdpFrame frame;
    RdpCertificate certificate;
    atomic_int cancelled;
    atomic_int connected;
    atomic_int frame_dirty;
    CRITICAL_SECTION graphics_lock;
    int surface_frame_open, order_frame_open;
    pSurfaceFrameMarker surface_marker;
    pFrameMarker order_marker;
    RdpgfxClientContext* gfx;
    DispClientContext* disp;
    BOOL disp_ready;
    UINT64 disp_max_area;
    UINT32 desired_width, desired_height, sent_width, sent_height, resize_requests;
    pcRdpgfxStartFrame gfx_start;
    pcRdpgfxEndFrame gfx_end;
    UINT64 last_frame_ms;
    atomic_uint_fast64_t paints;
    UINT64 complete_frames, publications, copy_us;
    UINT32 classic_codec;
    pSurfaceBits surface_bits;
    pBitmapUpdate bitmap_update;
    UINT32 last_codec;
    UINT32 channel_loads;
    pcRdpgfxSurfaceCommand gfx_command;
    int gdi_ready;
    uint8_t held_keys[512];
    uint16_t held_mouse,held_extended,last_x,last_y,last_unicode;
    int unicode_down;
};
typedef struct { rdpContext context; RdpBridge* bridge; } BridgeContext;
static RdpBridge* bridge_of(rdpContext* ctx) { return ((BridgeContext*)ctx)->bridge; }
int rdp_manager_initialize(void) {
    return winpr_InitializeSSL(WINPR_SSL_INIT_DEFAULT | WINPR_SSL_INIT_ENABLE_LOCKING)
        && freerdp_setApplicationDetails("RdpManager", "RdpManager", 1)
        && freerdp_register_addin_provider(freerdp_channels_load_static_addin_entry, 0) == 0;
}
static int publish_frame(RdpBridge* bridge, BOOL boundary);
static UINT gfx_start_frame(RdpgfxClientContext* gfx, const RDPGFX_START_FRAME_PDU* frame) {
    rdpGdi* gdi = gfx->custom;
    RdpBridge* bridge = bridge_of(gdi->context);
    EnterCriticalSection(&bridge->graphics_lock);
    UINT result = bridge->gfx_start(gfx, frame);
    LeaveCriticalSection(&bridge->graphics_lock);
    return result;
}
static UINT gfx_end_frame(RdpgfxClientContext* gfx, const RDPGFX_END_FRAME_PDU* frame) {
    rdpGdi* gdi = gfx->custom;
    RdpBridge* bridge = bridge_of(gdi->context);
    EnterCriticalSection(&bridge->graphics_lock);
    UINT result = bridge->gfx_end(gfx, frame);
    if (result == CHANNEL_RC_OK) {
        bridge->complete_frames++;
        if (!publish_frame(bridge, TRUE)) result = ERROR_INTERNAL_ERROR;
    }
    LeaveCriticalSection(&bridge->graphics_lock);
    return result;
}
static UINT gfx_surface_command(RdpgfxClientContext* gfx, const RDPGFX_SURFACE_COMMAND* command) {
    rdpGdi* gdi = gfx->custom;
    RdpBridge* bridge = bridge_of(gdi->context);
    EnterCriticalSection(&bridge->graphics_lock);
    bridge->last_codec = command->codecId;
    UINT result = bridge->gfx_command(gfx, command);
    LeaveCriticalSection(&bridge->graphics_lock);
    return result;
}
static UINT display_caps(DispClientContext* disp, UINT32 monitors, UINT32 a, UINT32 b) {
    RdpBridge* bridge = disp->custom;
    EnterCriticalSection(&bridge->graphics_lock);
    bridge->disp_max_area = (UINT64)a * b;
    bridge->disp_ready = monitors > 0 && bridge->disp_max_area > 0;
    LeaveCriticalSection(&bridge->graphics_lock);
    return CHANNEL_RC_OK;
}
static void send_desktop_size(RdpBridge* bridge) {
    EnterCriticalSection(&bridge->graphics_lock);
    UINT32 w = bridge->desired_width, h = bridge->desired_height;
    if (bridge->disp && bridge->disp_ready && w && h &&
        (w != bridge->sent_width || h != bridge->sent_height)) {
        if ((UINT64)w * h <= bridge->disp_max_area) {
            DISPLAY_CONTROL_MONITOR_LAYOUT monitor = {0};
            monitor.Flags = DISPLAY_CONTROL_MONITOR_PRIMARY;
            monitor.Width = w; monitor.Height = h;
            monitor.PhysicalWidth = (w * 254 + 480) / 960;
            monitor.PhysicalHeight = (h * 254 + 480) / 960;
            monitor.DesktopScaleFactor = 100; monitor.DeviceScaleFactor = 100;
            if (bridge->disp->SendMonitorLayout(bridge->disp, 1, &monitor) == CHANNEL_RC_OK)
                bridge->resize_requests++;
            else bridge->disp_ready = FALSE;
        }
        bridge->sent_width = w; bridge->sent_height = h;
    }
    LeaveCriticalSection(&bridge->graphics_lock);
}
static void channel_connected(void* context, const ChannelConnectedEventArgs* event) {
    rdpContext* ctx = context;
    if (strcmp(event->name, DISP_DVC_CHANNEL_NAME) == 0) {
        RdpBridge* bridge = bridge_of(ctx);
        EnterCriticalSection(&bridge->graphics_lock);
        bridge->disp = event->pInterface;
        bridge->disp->custom = bridge;
        bridge->disp->DisplayControlCaps = display_caps;
        bridge->disp_ready = FALSE;
        bridge->sent_width = bridge->sent_height = 0;
        LeaveCriticalSection(&bridge->graphics_lock);
        return;
    }
    if (strcmp(event->name, RDPGFX_DVC_CHANNEL_NAME) != 0) return;
    RdpBridge* bridge = bridge_of(ctx);
    EnterCriticalSection(&bridge->graphics_lock);
    RdpgfxClientContext* gfx = event->pInterface;
    if (!ctx->gdi || !gdi_graphics_pipeline_init(ctx->gdi, gfx)) {
        freerdp_set_last_error(ctx, FREERDP_ERROR_CONNECT_FAILED);
        LeaveCriticalSection(&bridge->graphics_lock);
        return;
    }
    bridge->gfx_start = gfx->StartFrame;
    bridge->gfx_end = gfx->EndFrame;
    bridge->gfx_command = gfx->SurfaceCommand;
    gfx->StartFrame = gfx_start_frame;
    gfx->EndFrame = gfx_end_frame;
    gfx->SurfaceCommand = gfx_surface_command;
    bridge->gfx = gfx;
    LeaveCriticalSection(&bridge->graphics_lock);
}
static void channel_disconnected(void* context, const ChannelDisconnectedEventArgs* event) {
    rdpContext* ctx = context;
    RdpBridge* bridge = bridge_of(ctx);
    EnterCriticalSection(&bridge->graphics_lock);
    if (strcmp(event->name, DISP_DVC_CHANNEL_NAME) == 0) { bridge->disp = NULL; bridge->disp_ready = FALSE; }
    if (strcmp(event->name, RDPGFX_DVC_CHANNEL_NAME) == 0 && bridge->gfx) {
        if (ctx->gdi) gdi_graphics_pipeline_uninit(ctx->gdi, bridge->gfx);
        bridge->gfx = NULL;
    }
    LeaveCriticalSection(&bridge->graphics_lock);
}
static BOOL pre_connect(freerdp* instance) {
    rdpContext* ctx = instance->context;
    if (atomic_load(&bridge_of(ctx)->cancelled)) return FALSE;
    if (PubSub_SubscribeChannelConnected(ctx->pubSub, channel_connected) < 0) return FALSE;
    if (PubSub_SubscribeChannelDisconnected(ctx->pubSub, channel_disconnected) < 0) return FALSE;
    return TRUE;
}
static BOOL load_channels(freerdp* instance) {
    rdpContext* ctx = instance->context;
    /* Core recreates the channel manager after PreConnect. Load only here. */
    if (!freerdp_settings_set_bool(ctx->settings, FreeRDP_AudioPlayback, FALSE) ||
        !freerdp_settings_set_bool(ctx->settings, FreeRDP_AudioCapture, FALSE) ||
        !freerdp_settings_set_bool(ctx->settings, FreeRDP_SupportVideoOptimized, FALSE) ||
        !freerdp_settings_set_bool(ctx->settings, FreeRDP_SupportGeometryTracking, FALSE) ||
        !freerdp_settings_set_bool(ctx->settings, FreeRDP_SupportDisplayControl, TRUE)) return FALSE;
    if (freerdp_static_channel_collection_find(ctx->settings, "rdpsnd") &&
        !freerdp_client_del_static_channel(ctx->settings, "rdpsnd")) return FALSE;
    if (freerdp_dynamic_channel_collection_find(ctx->settings, "rdpsnd") &&
        !freerdp_client_del_dynamic_channel(ctx->settings, "rdpsnd")) return FALSE;
    if (!freerdp_client_load_addins(ctx->channels, ctx->settings)) return FALSE;
    bridge_of(ctx)->channel_loads++;
    return TRUE;
}
static BOOL surface_marker(rdpContext* ctx, const SURFACE_FRAME_MARKER* marker) {
    RdpBridge* b = bridge_of(ctx);
    if (b->surface_marker && !b->surface_marker(ctx, marker)) return FALSE;
    b->surface_frame_open = marker->frameAction == SURFACECMD_FRAMEACTION_BEGIN;
    if (!b->surface_frame_open) { b->complete_frames++; return publish_frame(b, TRUE) != 0; }
    return TRUE;
}
static BOOL order_marker(rdpContext* ctx, const FRAME_MARKER_ORDER* marker) {
    RdpBridge* b = bridge_of(ctx);
    if (b->order_marker && !b->order_marker(ctx, marker)) return FALSE;
    b->order_frame_open = marker->action == FRAME_START;
    if (!b->order_frame_open) { b->complete_frames++; return publish_frame(b, TRUE) != 0; }
    return TRUE;
}

static BOOL begin_paint(rdpContext* ctx) {
    if (!ctx->gdi || !ctx->gdi->primary || !ctx->gdi->primary->hdc ||
        !ctx->gdi->primary->hdc->hwnd || !ctx->gdi->primary->hdc->hwnd->invalid) return FALSE;
    ctx->gdi->primary->hdc->hwnd->invalid->null = TRUE;
    return TRUE;
}
static BOOL end_paint(rdpContext* ctx) {
    RdpBridge* bridge = bridge_of(ctx);
    rdpGdi* gdi = ctx->gdi;
    if (!gdi || atomic_load(&bridge->cancelled)) return FALSE;
    if (gdi->primary->hdc->hwnd->invalid->null) return TRUE;
    atomic_fetch_add(&bridge->paints, 1);
    atomic_store(&bridge->frame_dirty, 1);
    return TRUE;
}
static BOOL desktop_resize(rdpContext* ctx) {
    const UINT32 w = freerdp_settings_get_uint32(ctx->settings, FreeRDP_DesktopWidth);
    const UINT32 h = freerdp_settings_get_uint32(ctx->settings, FreeRDP_DesktopHeight);
    if (!w || !h || w > 8192 || h > 8192 || (UINT64)w * h > 16777216) return FALSE;
    if(!gdi_resize(ctx->gdi,w,h))return FALSE;
    memset(ctx->gdi->primary_buffer,0,(size_t)ctx->gdi->stride*ctx->gdi->height);
    return TRUE;
}
/* Initial frontend uses GTK's local cursor; protocol cursor updates are acknowledged. */
static BOOL pointer_position(rdpContext* c,const POINTER_POSITION_UPDATE* p){(void)c;(void)p;return TRUE;}
static BOOL pointer_system(rdpContext* c,const POINTER_SYSTEM_UPDATE* p){(void)c;(void)p;return TRUE;}
static BOOL pointer_color(rdpContext* c,const POINTER_COLOR_UPDATE* p){(void)c;(void)p;return TRUE;}
static BOOL pointer_new(rdpContext* c,const POINTER_NEW_UPDATE* p){(void)c;(void)p;return TRUE;}
static BOOL pointer_cached(rdpContext* c,const POINTER_CACHED_UPDATE* p){(void)c;(void)p;return TRUE;}
static BOOL surface_bits(rdpContext* ctx, const SURFACE_BITS_COMMAND* cmd) {
    RdpBridge* b = bridge_of(ctx);
    UINT32 codec = cmd->bmp.codecID;
    b->last_codec = codec;
    b->classic_codec = codec == 0 ? 3 :
        codec == freerdp_settings_get_uint32(ctx->settings, FreeRDP_RemoteFxCodecId) ? 1 :
        codec == freerdp_settings_get_uint32(ctx->settings, FreeRDP_NSCodecId) ? 2 : 4;
    return b->surface_bits(ctx, cmd);
}
static BOOL bitmap_update(rdpContext* ctx, const BITMAP_UPDATE* update) {
    RdpBridge* b = bridge_of(ctx);
    b->classic_codec = 0; b->last_codec = 0;
    return b->bitmap_update(ctx, update);
}
static BOOL post_connect(freerdp* instance) {
    const UINT32 w=freerdp_settings_get_uint32(instance->context->settings,FreeRDP_DesktopWidth);
    const UINT32 h=freerdp_settings_get_uint32(instance->context->settings,FreeRDP_DesktopHeight);
    if(!w||!h||w>8192||h>8192||(UINT64)w*h>16777216)return FALSE;
    if (!gdi_init(instance, PIXEL_FORMAT_BGRX32)) return FALSE;
    RdpBridge* bridge = bridge_of(instance->context);
    bridge->gdi_ready = 1;
    memset(instance->context->gdi->primary_buffer,0,(size_t)instance->context->gdi->stride*instance->context->gdi->height);
    instance->context->update->BeginPaint = begin_paint;
    instance->context->update->EndPaint = end_paint;
    instance->context->update->DesktopResize = desktop_resize;
    bridge->surface_bits = instance->context->update->SurfaceBits;
    bridge->bitmap_update = instance->context->update->BitmapUpdate;
    instance->context->update->SurfaceBits = surface_bits;
    instance->context->update->BitmapUpdate = bitmap_update;
    bridge->surface_marker = instance->context->update->SurfaceFrameMarker;
    bridge->order_marker = instance->context->update->altsec->FrameMarker;
    instance->context->update->SurfaceFrameMarker = surface_marker;
    instance->context->update->altsec->FrameMarker = order_marker;
    instance->context->update->pointer->PointerPosition=pointer_position;
    instance->context->update->pointer->PointerSystem=pointer_system;
    instance->context->update->pointer->PointerColor=pointer_color;
    instance->context->update->pointer->PointerNew=pointer_new;
    instance->context->update->pointer->PointerCached=pointer_cached;
    return TRUE;
}
static void post_disconnect(freerdp* instance) {
    RdpBridge* bridge = bridge_of(instance->context);
    PubSub_UnsubscribeChannelConnected(instance->context->pubSub, channel_connected);
    PubSub_UnsubscribeChannelDisconnected(instance->context->pubSub, channel_disconnected);
    EnterCriticalSection(&bridge->graphics_lock);
    if (bridge->gfx && instance->context->gdi) {
        gdi_graphics_pipeline_uninit(instance->context->gdi, bridge->gfx);
        bridge->gfx = NULL;
    }
    if (bridge->gdi_ready) { gdi_free(instance); bridge->gdi_ready = 0; }
    LeaveCriticalSection(&bridge->graphics_lock);
}
static BOOL authenticate(freerdp* instance, char** user, char** pass, char** domain, rdp_auth_reason reason) {
    (void)instance; (void)domain; (void)reason;
    /* Credentials were collected in GTK before starting this worker. Never prompt on stdin. */
    return user && *user && pass && *pass;
}
static char* fingerprint(const char* data, DWORD flags) {
    if (!data) return NULL;
    if (!(flags & VERIFY_CERT_FLAG_FP_IS_PEM)) {
        size_t size = strlen(data) + 1;
        char* copy = malloc(size);
        if (copy) memcpy(copy, data, size);
        return copy;
    }
    rdpCertificate* cert = freerdp_certificate_new_from_pem(data);
    if (!cert) return NULL;
    char* hash = freerdp_certificate_get_fingerprint_by_hash(cert, "sha256");
    freerdp_certificate_free(cert);
    return hash;
}
static DWORD verify_certificate(freerdp* instance, const char* host, UINT16 port,
    const char* common, const char* subject, const char* issuer, const char* data, DWORD flags) {
    RdpBridge* bridge = bridge_of(instance->context);
    char* hash = fingerprint(data, flags);
    if (!hash) return 0;
    const int accepted = bridge->certificate(bridge->opaque, 0, host, port, common, subject, issuer, hash, "");
    free(hash);
    return accepted == 2 ? 1 : (accepted == 1 ? 2 : 0); /* FreeRDP: 1 save host/port trust, 2 session-only */
}
static DWORD verify_changed_certificate(freerdp* instance, const char* host, UINT16 port,
    const char* common, const char* subject, const char* issuer, const char* data,
    const char* old_subject, const char* old_issuer, const char* old_data, DWORD flags) {
    (void)old_subject; (void)old_issuer;
    RdpBridge* bridge = bridge_of(instance->context);
    char* hash = fingerprint(data, flags);
    char* old_hash = fingerprint(old_data, flags);
    bridge->certificate(bridge->opaque, 1, host, port, common, subject, issuer, hash ? hash : "", old_hash ? old_hash : "");
    free(hash); free(old_hash);
    return 0; /* a changed cached certificate is never accepted by this callback */
}

RdpBridge* rdp_manager_new(const char* host, uint16_t port, const char* user,
    const char* password, const char* domain, uint32_t width, uint32_t height,
    uint32_t keyboard_layout, uint32_t quality, void* opaque, RdpFrame frame, RdpCertificate certificate) {
    if (!host || !user || !password || !domain || !frame || !certificate || !port ||
        !width || !height || width > 8192 || height > 8192 || (uint64_t)width * height > 16777216) return NULL;
    RdpBridge* bridge = calloc(1, sizeof(*bridge));
    if (!bridge) return NULL;
    atomic_init(&bridge->cancelled, 0);
    atomic_init(&bridge->connected, 0);
    atomic_init(&bridge->frame_dirty, 0);
    atomic_init(&bridge->paints, 0);
    atomic_init(&bridge->input_wakes, 0);
    atomic_init(&bridge->input_scans, 0);
    atomic_init(&bridge->input_unicode, 0);
    InitializeCriticalSection(&bridge->graphics_lock);
    bridge->opaque = opaque; bridge->frame = frame; bridge->certificate = certificate;
    bridge->instance = freerdp_new();
    if (!bridge->instance) { DeleteCriticalSection(&bridge->graphics_lock); free(bridge); return NULL; }
    freerdp* instance = bridge->instance;
    instance->ContextSize = sizeof(BridgeContext);
    if (!freerdp_context_new(instance)) { freerdp_free(instance); DeleteCriticalSection(&bridge->graphics_lock); free(bridge); return NULL; }
    ((BridgeContext*)instance->context)->bridge = bridge;
    instance->PostConnect = post_connect;
    instance->PreConnect = pre_connect;
    instance->LoadChannels = load_channels;
    instance->PostDisconnect = post_disconnect;
    instance->AuthenticateEx = authenticate;
    instance->VerifyCertificateEx = verify_certificate;
    instance->VerifyChangedCertificateEx = verify_changed_certificate;
    bridge->input_event = CreateEvent(NULL, TRUE, FALSE, NULL);
    if (!bridge->input_event) goto fail;
    rdpSettings* settings = instance->context->settings;
#define SET_STRING(key, value) if (!freerdp_settings_set_string(settings, key, value)) goto fail
#define SET_BOOL(key, value) if (!freerdp_settings_set_bool(settings, key, value)) goto fail
#define SET_UINT(key, value) if (!freerdp_settings_set_uint32(settings, key, value)) goto fail
    SET_STRING(FreeRDP_ServerHostname, host);
    SET_STRING(FreeRDP_Username, user);
    SET_STRING(FreeRDP_Password, password);
    SET_STRING(FreeRDP_Domain, domain);
    SET_UINT(FreeRDP_ServerPort, port);
    SET_UINT(FreeRDP_DesktopWidth, width);
    SET_UINT(FreeRDP_DesktopHeight, height);
    SET_UINT(FreeRDP_ColorDepth, 32);
    SET_UINT(FreeRDP_KeyboardLayout, keyboard_layout);
    SET_UINT(FreeRDP_KeyboardType, 4);
    SET_UINT(FreeRDP_KeyboardSubType, 0);
    SET_UINT(FreeRDP_KeyboardFunctionKey, 12);
    SET_UINT(FreeRDP_TcpConnectTimeout, 10000);
    SET_BOOL(FreeRDP_TlsSecurity, TRUE);
    SET_BOOL(FreeRDP_NlaSecurity, TRUE);
    SET_BOOL(FreeRDP_RdpSecurity, FALSE);
    SET_BOOL(FreeRDP_IgnoreCertificate, FALSE);
    SET_BOOL(FreeRDP_AutoAcceptCertificate, FALSE);
    SET_BOOL(FreeRDP_AutoDenyCertificate, FALSE);
    SET_BOOL(FreeRDP_CertificateCallbackPreferPEM, TRUE);
    SET_BOOL(FreeRDP_SupportGraphicsPipeline, quality != 3);
    /* Capability 8.1 advertises AVC420. In the AVC444 preset omit it, while
       keeping version 8 and 10+ so a server can choose a non-AVC fallback. */
    SET_UINT(FreeRDP_GfxCapsFilter, quality == 4 ? (1u << 1) : 0);
    const char* build = freerdp_get_build_config();
    const BOOL h264 = quality != 1 && quality != 3 && build && strstr(build, "WITH_GFX_H264=ON");
    const char* decoder_mode = getenv("RDP_MANAGER_H264_DECODER");
    const BOOL hw_request = quality != 2 && h264 && strstr(build, "WITH_FFMPEG_HWACCEL=ON") &&
        (!decoder_mode || strcmp(decoder_mode, "software") != 0);
    /* GDI initialization stays ours; FreeRDP uses this setting to request GPU H264.
       The framebuffer still passes through CPU memory, not a zero-copy renderer. */
    SET_BOOL(FreeRDP_SoftwareGdi, !hw_request);
    SET_BOOL(FreeRDP_GfxH264, h264);
    SET_BOOL(FreeRDP_GfxAVC444, h264);
    SET_BOOL(FreeRDP_GfxAVC444v2, h264);
    SET_BOOL(FreeRDP_GfxProgressive, TRUE);
    SET_BOOL(FreeRDP_AltSecFrameMarkerSupport, TRUE);
    SET_BOOL(FreeRDP_FrameMarkerCommandEnabled, TRUE);
    SET_BOOL(FreeRDP_AudioPlayback, FALSE);
    SET_BOOL(FreeRDP_RedirectDrives, FALSE);
    SET_BOOL(FreeRDP_RedirectSmartCards, FALSE);
    SET_BOOL(FreeRDP_RemoteFxCodec, quality != 3);
    SET_BOOL(FreeRDP_NSCodec, quality != 3);
    if (quality == 3) {
        SET_BOOL(FreeRDP_RemoteFxImageCodec, FALSE);
        SET_BOOL(FreeRDP_JpegCodec, FALSE);
        SET_UINT(FreeRDP_BitmapCacheV3CodecId, 0);
    }
    SET_BOOL(FreeRDP_RedirectClipboard, FALSE);
    SET_BOOL(FreeRDP_DynamicResolutionUpdate, TRUE);
    SET_BOOL(FreeRDP_DesktopResize, TRUE);
    SET_BOOL(FreeRDP_AsyncUpdate, FALSE);
    SET_BOOL(FreeRDP_AsyncChannels, FALSE);
    SET_BOOL(FreeRDP_BitmapCachePersistEnabled, FALSE);
    SET_BOOL(FreeRDP_TransportDump, FALSE);
    return bridge;
fail:
    rdp_manager_free(bridge);
    return NULL;
}
int rdp_manager_connect(RdpBridge* bridge) {
    if (atomic_load(&bridge->cancelled)) return 0;
    int result = freerdp_connect(bridge->instance) ? 1 : 0;
    atomic_store(&bridge->connected, result);
    return result;
}
int rdp_manager_poll(RdpBridge* bridge) {
    rdpContext* ctx = bridge->instance->context;
    if (atomic_load(&bridge->cancelled) || freerdp_shall_disconnect_context(ctx)) return 0;
    send_desktop_size(bridge);
    HANDLE handles[64] = { 0 };
    DWORD count = freerdp_get_event_handles(ctx, handles, 63);
    if (!count) return -1;
    DWORD input_index = count;
    handles[count++] = bridge->input_event;
    DWORD result = WaitForMultipleObjects(count, handles, FALSE, 15);
    if (result == WAIT_FAILED) return -1;
    if (result == WAIT_OBJECT_0 + input_index) atomic_fetch_add(&bridge->input_wakes, 1);
    if (result != WAIT_TIMEOUT && result != WAIT_OBJECT_0 + input_index && !freerdp_check_event_handles(ctx)) return 0;
    return publish_frame(bridge, FALSE);
}
static int publish_frame(RdpBridge* bridge, BOOL boundary) {
    rdpContext* ctx = bridge->instance->context;
    UINT64 now = GetTickCount64();
    EnterCriticalSection(&bridge->graphics_lock);
    RdpgfxClientContext* gfx = bridge->gfx;
    if (gfx) EnterCriticalSection(&gfx->mux);
    int accepted = 2;
    if (atomic_load(&bridge->frame_dirty) && bridge->gdi_ready && !ctx->gdi->inGfxFrame
        && !bridge->surface_frame_open && !bridge->order_frame_open && (boundary || now - bridge->last_frame_ms >= 16)) {
        rdpGdi* gdi = ctx->gdi;
        UINT64 started = winpr_GetTickCount64NS();
        accepted = bridge->frame(bridge->opaque, gdi->primary_buffer,
                                gdi->width, gdi->height, gdi->stride);
        bridge->copy_us += (winpr_GetTickCount64NS() - started) / 1000;
        if (accepted == 1) { bridge->publications++; atomic_store(&bridge->frame_dirty, 0); bridge->last_frame_ms = now; }
    }
    if (gfx) LeaveCriticalSection(&gfx->mux);
    LeaveCriticalSection(&bridge->graphics_lock);
    if (!accepted) return 0;
    return 1;
}
int rdp_manager_input(RdpBridge* bridge, uint32_t kind, uint16_t flags, uint16_t a, uint16_t b) {
    if (atomic_load(&bridge->cancelled)) return 0;
    rdpInput* input = bridge->instance->context->input;
    switch (kind) {
        case 0: {
            atomic_fetch_add(&bridge->input_scans, 1);
            if(!freerdp_input_send_keyboard_event(input,flags,(UINT8)a))return 0;
            bridge->held_keys[(a&0xFF)+((flags&KBD_FLAGS_EXTENDED)?256:0)]=(flags&KBD_FLAGS_RELEASE)?0:1;
            return 1;
        }
        case 1: {
            atomic_fetch_add(&bridge->input_unicode, 1);
            if(!freerdp_input_send_unicode_keyboard_event(input,flags,a))return 0;
            bridge->unicode_down=(flags&KBD_FLAGS_RELEASE)?0:1;bridge->last_unicode=a;return 1;
        }
        case 2: {
            if(!freerdp_input_send_mouse_event(input,flags,a,b))return 0;
            bridge->last_x=a;bridge->last_y=b;
            if(flags&PTR_FLAGS_DOWN)bridge->held_mouse|=flags&0x7000;
            else bridge->held_mouse&=~(flags&0x7000);
            return 1;
        }
        case 3: {
            if(!freerdp_input_send_extended_mouse_event(input,flags,a,b))return 0;
            bridge->last_x=a;bridge->last_y=b;
            if(flags&PTR_XFLAGS_DOWN)bridge->held_extended|=flags&3;
            else bridge->held_extended&=~(flags&3);
            return 1;
        }
        case 4: return freerdp_input_send_keyboard_pause_event(input);
        case 5: return freerdp_input_send_focus_in_event(input, flags);
        case 6:
            if (a < 200 || b < 200 || a > 8192 || b > 8192 || (UINT64)a*b > 16777216) return 1;
            EnterCriticalSection(&bridge->graphics_lock);
            bridge->desired_width = a & ~1u; bridge->desired_height = b & ~1u;
            LeaveCriticalSection(&bridge->graphics_lock);
            return 1;
        default: return 0;
    }
}
void rdp_manager_prepare_input(RdpBridge* bridge) { (void)ResetEvent(bridge->input_event); }
void rdp_manager_wake(RdpBridge* bridge) { (void)SetEvent(bridge->input_event); }
void rdp_manager_cancel(RdpBridge* bridge) {
    rdp_manager_wake(bridge);
    atomic_store(&bridge->cancelled, 1);
    /* Active sessions leave the bounded poll loop and release input before disconnect. */
    if (!atomic_load(&bridge->connected))
        (void)freerdp_abort_connect_context(bridge->instance->context);
}
uint32_t rdp_manager_error(RdpBridge* bridge) { return freerdp_get_last_error(bridge->instance->context); }
const char* rdp_manager_error_name(RdpBridge* bridge) { return freerdp_get_last_error_name(rdp_manager_error(bridge)); }
void rdp_manager_free(RdpBridge* bridge) {
    if (!bridge) return;
    if (bridge->instance) {
        if (bridge->instance->context) {
            rdpInput* input=bridge->instance->context->input;
            if(bridge->gdi_ready&&input&&!freerdp_shall_disconnect_context(bridge->instance->context)){
                for(size_t key=0;key<512;key++)if(bridge->held_keys[key])
                    (void)freerdp_input_send_keyboard_event(input,KBD_FLAGS_RELEASE|(key>=256?KBD_FLAGS_EXTENDED:0),(UINT8)key);
                if(bridge->unicode_down)(void)freerdp_input_send_unicode_keyboard_event(input,KBD_FLAGS_RELEASE,bridge->last_unicode);
                for(UINT16 flag=0x1000;flag<=0x4000;flag<<=1)if(bridge->held_mouse&flag)
                    (void)freerdp_input_send_mouse_event(input,flag,bridge->last_x,bridge->last_y);
                for(UINT16 flag=1;flag<=2;flag<<=1)if(bridge->held_extended&flag)
                    (void)freerdp_input_send_extended_mouse_event(input,flag,bridge->last_x,bridge->last_y);
            }
            (void)freerdp_disconnect(bridge->instance);
            if (bridge->gdi_ready) { gdi_free(bridge->instance); bridge->gdi_ready = 0; }
            char* pass = freerdp_settings_get_string_writable(bridge->instance->context->settings, FreeRDP_Password);
            if (pass) SecureZeroMemory(pass, strlen(pass));
            freerdp_context_free(bridge->instance);
        }
        freerdp_free(bridge->instance);
    }
    if (bridge->input_event) CloseHandle(bridge->input_event);
    DeleteCriticalSection(&bridge->graphics_lock);
    free(bridge);
}

const char* rdp_manager_build_config(void) { return freerdp_get_build_config(); }


void rdp_manager_graphics_stats(RdpBridge* bridge, char* output, size_t capacity) {
    EnterCriticalSection(&bridge->graphics_lock);
    RdpgfxClientContext* gfx = bridge->gfx;
    if (gfx) EnterCriticalSection(&gfx->mux);
    rdpSettings* settings = bridge->instance->context->settings;
    const char* classic[] = {"bitmap", "RemoteFX", "NSCodec", "raw-surface", "other-surface"};
    snprintf(output, capacity,
        "mode=%s channel_loads=%u classic_codec=%s codec_id=%u size=%ux%u keyboard_layout=%u paints=%llu frames=%llu copies=%llu copy_us=%llu input_wakes=%llu scans=%llu unicode=%llu hw_requested=%d disp_ready=%d resize_requests=%u rfx=%d nsc=%d",
        bridge->gfx ? "EGFX" : "classic", bridge->channel_loads, bridge->gfx ? "-" : classic[bridge->classic_codec], bridge->last_codec,
        freerdp_settings_get_uint32(settings, FreeRDP_DesktopWidth),
        freerdp_settings_get_uint32(settings, FreeRDP_DesktopHeight),
        freerdp_settings_get_uint32(settings, FreeRDP_KeyboardLayout),
        (unsigned long long)atomic_load(&bridge->paints), (unsigned long long)bridge->complete_frames,
        (unsigned long long)bridge->publications, (unsigned long long)bridge->copy_us, (unsigned long long)atomic_load(&bridge->input_wakes), (unsigned long long)atomic_load(&bridge->input_scans), (unsigned long long)atomic_load(&bridge->input_unicode), !freerdp_settings_get_bool(settings, FreeRDP_SoftwareGdi), bridge->disp_ready, bridge->resize_requests,
        freerdp_settings_get_bool(settings, FreeRDP_RemoteFxCodec),
        freerdp_settings_get_bool(settings, FreeRDP_NSCodec));
    if (gfx) LeaveCriticalSection(&gfx->mux);
    LeaveCriticalSection(&bridge->graphics_lock);
}
