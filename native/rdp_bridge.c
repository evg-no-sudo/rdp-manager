#include "rdp_bridge.h"
#include <freerdp/freerdp.h>
#include <freerdp/settings.h>
#include <freerdp/input.h>
#include <freerdp/update.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/codec/color.h>
#include <freerdp/crypto/certificate.h>
#include <freerdp/utils/helpers.h>
#include <winpr/synch.h>
#include <winpr/sysinfo.h>
#include <winpr/ssl.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

struct RdpBridge {
    freerdp* instance;
    void* opaque;
    RdpFrame frame;
    RdpCertificate certificate;
    atomic_int cancelled;
    atomic_int connected;
    int frame_dirty;
    UINT64 last_frame_ms;
    int gdi_ready;
    uint8_t held_keys[512];
    uint16_t held_mouse,held_extended,last_x,last_y,last_unicode;
    int unicode_down;
};
typedef struct { rdpContext context; RdpBridge* bridge; } BridgeContext;
static RdpBridge* bridge_of(rdpContext* ctx) { return ((BridgeContext*)ctx)->bridge; }
int rdp_manager_initialize(void) {
    return winpr_InitializeSSL(WINPR_SSL_INIT_DEFAULT | WINPR_SSL_INIT_ENABLE_LOCKING)
        && freerdp_setApplicationDetails("RdpManager", "RdpManager", 1);
}
static BOOL pre_connect(freerdp* instance) { return !atomic_load(&bridge_of(instance->context)->cancelled); }

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
    bridge->frame_dirty = 1;
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
    instance->context->update->pointer->PointerPosition=pointer_position;
    instance->context->update->pointer->PointerSystem=pointer_system;
    instance->context->update->pointer->PointerColor=pointer_color;
    instance->context->update->pointer->PointerNew=pointer_new;
    instance->context->update->pointer->PointerCached=pointer_cached;
    return TRUE;
}
static void post_disconnect(freerdp* instance) {
    RdpBridge* bridge = bridge_of(instance->context);
    if (bridge->gdi_ready) { gdi_free(instance); bridge->gdi_ready = 0; }
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
    return accepted ? 2 : 0; /* explicit, session-only approval; never writes a trust exception */
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
    void* opaque, RdpFrame frame, RdpCertificate certificate) {
    if (!host || !user || !password || !domain || !frame || !certificate || !port ||
        !width || !height || width > 8192 || height > 8192 || (uint64_t)width * height > 16777216) return NULL;
    RdpBridge* bridge = calloc(1, sizeof(*bridge));
    if (!bridge) return NULL;
    atomic_init(&bridge->cancelled, 0);
    atomic_init(&bridge->connected, 0);
    bridge->opaque = opaque; bridge->frame = frame; bridge->certificate = certificate;
    bridge->instance = freerdp_new();
    if (!bridge->instance) { free(bridge); return NULL; }
    freerdp* instance = bridge->instance;
    instance->ContextSize = sizeof(BridgeContext);
    if (!freerdp_context_new(instance)) { freerdp_free(instance); free(bridge); return NULL; }
    ((BridgeContext*)instance->context)->bridge = bridge;
    instance->PostConnect = post_connect;
    instance->PreConnect = pre_connect;
    instance->PostDisconnect = post_disconnect;
    instance->AuthenticateEx = authenticate;
    instance->VerifyCertificateEx = verify_certificate;
    instance->VerifyChangedCertificateEx = verify_changed_certificate;
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
    SET_UINT(FreeRDP_TcpConnectTimeout, 10000);
    SET_BOOL(FreeRDP_TlsSecurity, TRUE);
    SET_BOOL(FreeRDP_NlaSecurity, TRUE);
    SET_BOOL(FreeRDP_RdpSecurity, FALSE);
    SET_BOOL(FreeRDP_IgnoreCertificate, FALSE);
    SET_BOOL(FreeRDP_AutoAcceptCertificate, FALSE);
    SET_BOOL(FreeRDP_AutoDenyCertificate, FALSE);
    SET_BOOL(FreeRDP_CertificateCallbackPreferPEM, TRUE);
    SET_BOOL(FreeRDP_SoftwareGdi, TRUE);
    SET_BOOL(FreeRDP_SupportGraphicsPipeline, FALSE);
    SET_BOOL(FreeRDP_RemoteFxCodec, FALSE);
    SET_BOOL(FreeRDP_NSCodec, FALSE);
    SET_BOOL(FreeRDP_RedirectClipboard, FALSE);
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
    HANDLE handles[64] = { 0 };
    DWORD count = freerdp_get_event_handles(ctx, handles, 64);
    if (!count) return -1;
    DWORD result = WaitForMultipleObjects(count, handles, FALSE, 15);
    if (result == WAIT_FAILED) return -1;
    if (result != WAIT_TIMEOUT && !freerdp_check_event_handles(ctx)) return 0;
    /* EndPaint may run hundreds of times per protocol batch. Publish only the
       final framebuffer, at most 60 times/sec, and retain dirty state if GTK
       has not consumed the previous frame yet. No sleep in paint callbacks. */
    UINT64 now = GetTickCount64();
    if (bridge->frame_dirty && bridge->gdi_ready && now - bridge->last_frame_ms >= 16) {
        rdpGdi* gdi = ctx->gdi;
        int accepted = bridge->frame(bridge->opaque, gdi->primary_buffer,
                                    gdi->width, gdi->height, gdi->stride);
        if (!accepted) return 0;
        if (accepted == 1) { bridge->frame_dirty = 0; bridge->last_frame_ms = now; }
    }
    return 1;
}
int rdp_manager_input(RdpBridge* bridge, uint32_t kind, uint16_t flags, uint16_t a, uint16_t b) {
    if (atomic_load(&bridge->cancelled)) return 0;
    rdpInput* input = bridge->instance->context->input;
    switch (kind) {
        case 0: {
            if(!freerdp_input_send_keyboard_event(input,flags,(UINT8)a))return 0;
            bridge->held_keys[(a&0xFF)+((flags&KBD_FLAGS_EXTENDED)?256:0)]=(flags&KBD_FLAGS_RELEASE)?0:1;
            return 1;
        }
        case 1: {
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
        default: return 0;
    }
}
void rdp_manager_cancel(RdpBridge* bridge) {
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
    free(bridge);
}
