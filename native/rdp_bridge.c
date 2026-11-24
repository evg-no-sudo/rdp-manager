#include "rdp_bridge.h"
#include <freerdp/freerdp.h>
#include <freerdp/settings.h>
#include <freerdp/input.h>
#include <freerdp/update.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/gdi/gfx.h>
#include <freerdp/client/rdpgfx.h>
#include <freerdp/client/disp.h>
#include <freerdp/client/cliprdr.h>
#include <freerdp/client/client_cliprdr_file.h>
#include <freerdp/client/channels.h>
#include <freerdp/client/cmdline.h>
#include <freerdp/event.h>
#include <freerdp/codec/color.h>
#include <freerdp/crypto/certificate.h>
#include <freerdp/utils/helpers.h>
#include <winpr/synch.h>
#include <winpr/sysinfo.h>
#include <winpr/ssl.h>
#include <winpr/user.h>
#include <winpr/string.h>
#include <winpr/file.h>
#include <winpr/shell.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

/* FreeRDP 3.32 uses a single FUSE mount path per process. */
static atomic_bool clipboard_mount_in_use;

struct RdpBridge {
    freerdp* instance;
    HANDLE input_event;
    atomic_uint_fast64_t input_wakes, input_scans, input_unicode;
    void* opaque;
    RdpFrame frame;
    RdpCertificate certificate;
    RdpClipboard clipboard_callback;
    CliprdrClientContext* clipboard;
    BOOL clipboard_ready;
    CliprdrFileContext* remote_files;
    wClipboard* remote_clipboard;
    RdpClipboard files_callback;
    UINT32 requested_format, offered_format;
    BOOL requested_files, offered_files, request_superseded;
    char* clipboard_text;
    RdpClipboardFile* clipboard_files;
    UINT32 clipboard_file_count;
    CRITICAL_SECTION clipboard_lock;
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
static void clipboard_trace(const char* message) {
    if (getenv("RDP_MANAGER_TRACE_CLIPBOARD"))
        fprintf(stderr, "[rdp-clipboard] %s\n", message);
}
void rdp_manager_clipboard_receive_files(RdpBridge* bridge, RdpClipboard callback) {
    bridge->files_callback = callback;
}
static UINT clipboard_capabilities(CliprdrClientContext* context, const CLIPRDR_CAPABILITIES* caps) {
    RdpBridge* bridge = bridge_of(context->rdpcontext);
    if (!bridge->remote_files || !caps->cCapabilitiesSets || !caps->capabilitySets)
        return CHANNEL_RC_OK;
    const CLIPRDR_CAPABILITY_SET* set = caps->capabilitySets;
    if (set->capabilitySetType == CB_CAPSTYPE_GENERAL &&
        set->capabilitySetLength >= CB_CAPSTYPE_GENERAL_LEN) {
        const CLIPRDR_GENERAL_CAPABILITY_SET* general = (const void*)set;
        if (!cliprdr_file_context_remote_set_flags(bridge->remote_files, general->generalFlags))
            return ERROR_INTERNAL_ERROR;
    }
    return CHANNEL_RC_OK;
}
static RdpBridge* clipboard_bridge(CliprdrClientContext* context) {
    return context->rdpcontext ? bridge_of(context->rdpcontext) : context->custom;
}
static UINT clipboard_request_offer(CliprdrClientContext* context) {
    RdpBridge* bridge = clipboard_bridge(context);
    if (bridge->requested_format || !bridge->offered_format) return CHANNEL_RC_OK;
    bridge->requested_format = bridge->offered_format;
    bridge->requested_files = bridge->offered_files;
    bridge->request_superseded = FALSE;
    bridge->offered_format = 0;
    CLIPRDR_FORMAT_DATA_REQUEST request = { .common.msgType = CB_FORMAT_DATA_REQUEST,
        .requestedFormatId = bridge->requested_format };
    return context->ClientFormatDataRequest(context, &request);
}
static UINT clipboard_advertise(RdpBridge* bridge) {
    if (!bridge->clipboard || !bridge->clipboard_ready) return CHANNEL_RC_OK;
    CLIPRDR_FORMAT format = bridge->clipboard_files
        ? (CLIPRDR_FORMAT){ .formatId = 0xC001, .formatName = "FileGroupDescriptorW" }
        : (CLIPRDR_FORMAT){ .formatId = CF_UNICODETEXT };
    CLIPRDR_FORMAT_LIST list = { .common = { .msgType = CB_FORMAT_LIST },
                                 .numFormats = bridge->clipboard_text || bridge->clipboard_files ? 1 : 0,
                                 .formats = bridge->clipboard_text || bridge->clipboard_files ? &format : NULL };
    return bridge->clipboard->ClientFormatList(bridge->clipboard, &list);
}
static UINT clipboard_ready(CliprdrClientContext* context, const CLIPRDR_MONITOR_READY* ready) {
    (void)ready;
    RdpBridge* bridge = clipboard_bridge(context);
    CLIPRDR_GENERAL_CAPABILITY_SET general = {
        .capabilitySetType = CB_CAPSTYPE_GENERAL,
        .capabilitySetLength = CB_CAPSTYPE_GENERAL_LEN,
        .version = CB_CAPS_VERSION_2,
        .generalFlags = CB_USE_LONG_FORMAT_NAMES | CB_STREAM_FILECLIP_ENABLED |
                        CB_FILECLIP_NO_FILE_PATHS
    };
    CLIPRDR_CAPABILITIES caps = { .common = { .msgType = CB_CLIP_CAPS },
        .cCapabilitiesSets = 1, .capabilitySets = (CLIPRDR_CAPABILITY_SET*)&general };
    UINT result = context->ClientCapabilities(context, &caps);
    if (result == CHANNEL_RC_OK) bridge->clipboard_ready = TRUE;
    return result == CHANNEL_RC_OK ? clipboard_advertise(bridge) : result;
}
static UINT clipboard_server_list(CliprdrClientContext* context, const CLIPRDR_FORMAT_LIST* list) {
    CLIPRDR_FORMAT_LIST_RESPONSE response = { .common = {
        .msgType = CB_FORMAT_LIST_RESPONSE, .msgFlags = CB_RESPONSE_OK } };
    UINT result = context->ClientFormatListResponse(context, &response);
    if (result != CHANNEL_RC_OK) return result;
    RdpBridge* bridge = clipboard_bridge(context);
    bridge->offered_format = 0;
    bridge->offered_files = FALSE;
    bridge->request_superseded = bridge->requested_format != 0;
    if (bridge->remote_files) {
        bridge->files_callback(bridge->opaque, "");
        result = cliprdr_file_context_notify_new_server_format_list(bridge->remote_files);
        if (result != CHANNEL_RC_OK) return result;
    }
    for (UINT32 i = 0; i < list->numFormats; i++) {
        const CLIPRDR_FORMAT* format = &list->formats[i];
        if (format->formatId && format->formatName &&
            strcmp(format->formatName, "FileGroupDescriptorW") == 0) {
            if (!bridge->remote_files) {
                clipboard_trace("server offers files but local FUSE receiver is unavailable");
                continue;
            }
            clipboard_trace("requesting remote file list");
            bridge->offered_format = format->formatId;
            bridge->offered_files = TRUE;
            break;
        }
        if (format->formatId == CF_UNICODETEXT) bridge->offered_format = CF_UNICODETEXT;
    }
    return clipboard_request_offer(context);
}
static BOOL clipboard_valid_file_list(const BYTE* data, UINT32 size) {
    if (size < 4 || size > 4 + 4096 * sizeof(FILEDESCRIPTORW)) return FALSE;
    FILEDESCRIPTORW* files = NULL;
    UINT32 count = 0;
    if (cliprdr_parse_file_list(data, size, &files, &count) != CHANNEL_RC_OK) {
        free(files);
        return FALSE;
    }
    BOOL valid = count > 0;
    for (UINT32 i = 0; valid && i < count; i++) {
        const WCHAR* name = files[i].cFileName;
        if (!name[0] || name[0] == '\\') valid = FALSE;
        size_t j = 0, start = 0;
        for (; j < ARRAYSIZE(files[i].cFileName) && name[j]; j++) {
            if (name[j] == '\\') {
                if (j == start || (name[start] == '.' &&
                    (j == start + 1 || (j == start + 2 && name[start + 1] == '.'))))
                    valid = FALSE;
                start = j + 1;
            }
            if (name[j] < 32 || name[j] == '/' || name[j] == ':') {
                valid = FALSE;
                break;
            }
        }
        if (j == ARRAYSIZE(files[i].cFileName) || j == start ||
            (name[start] == '.' && (j == start + 1 ||
                (j == start + 2 && name[start + 1] == '.')))) valid = FALSE;
    }
    free(files);
    return valid;
}
static UINT clipboard_server_data(CliprdrClientContext* context, const CLIPRDR_FORMAT_DATA_RESPONSE* response) {
    RdpBridge* bridge = clipboard_bridge(context);
    UINT32 format = bridge->requested_format;
    bridge->requested_format = 0;
    if (!format || bridge->request_superseded || !(response->common.msgFlags & CB_RESPONSE_OK) ||
        !response->requestedFormatData)
        return clipboard_request_offer(context);
    UINT32 size = response->common.dataLen;
    if (bridge->requested_files)
        clipboard_trace("received remote file list response");
    if (bridge->requested_files && clipboard_valid_file_list(response->requestedFormatData, size)) {
        wClipboard* clipboard = bridge->remote_clipboard;
        if (cliprdr_file_context_update_server_data(bridge->remote_files, clipboard,
                response->requestedFormatData, size)) {
            const char* base = ClipboardGetDelegate(clipboard)->basePath;
            FILEDESCRIPTORW* files = NULL;
            UINT32 count = 0;
            if (base && !strchr(base, '\n') &&
                cliprdr_parse_file_list(response->requestedFormatData, size, &files, &count) == CHANNEL_RC_OK) {
                char* paths = NULL;
                size_t length = 0;
                FILE* output = open_memstream(&paths, &length);
                if (output) {
                    for (UINT32 i = 0; i < count; i++) {
                        const WCHAR* wide = files[i].cFileName;
                        size_t j = 0;
                        while (wide[j] && wide[j] != '\\') j++;
                        if (wide[j]) continue;
                        char* name = ConvertWCharToUtf8Alloc(wide, NULL);
                        if (name) fprintf(output, "%s/%s\n", base, name);
                        free(name);
                    }
                    if (fclose(output) == 0 && length && length <= 1024 * 1024)
                        bridge->files_callback(bridge->opaque, paths);
                    else clipboard_trace("could not generate local file paths");
                    free(paths);
                }
            }
            free(files);
        } else clipboard_trace("could not expose remote files through FUSE");
    } else if (!bridge->requested_files && size >= 2 && size <= 2 * 1024 * 1024 && size % 2 == 0) {
        WCHAR* wide = calloc(size / 2 + 1, sizeof(WCHAR));
        if (!wide) return CHANNEL_RC_NO_MEMORY;
        memcpy(wide, response->requestedFormatData, size);
        char* utf8 = ConvertWCharToUtf8Alloc(wide, NULL);
        free(wide);
        if (utf8) { bridge->clipboard_callback(bridge->opaque, utf8); free(utf8); }
    }
    else if (bridge->requested_files) clipboard_trace("invalid remote file list");
    return CHANNEL_RC_OK;
}
static UINT clipboard_server_request(CliprdrClientContext* context, const CLIPRDR_FORMAT_DATA_REQUEST* request) {
    RdpBridge* bridge = clipboard_bridge(context);
    CLIPRDR_FORMAT_DATA_RESPONSE response = { .common = { .msgType = CB_FORMAT_DATA_RESPONSE } };
    WCHAR* wide = NULL;
    BYTE* file_data = NULL;
    UINT32 file_length = 0;
    size_t chars = 0;
    EnterCriticalSection(&bridge->clipboard_lock);
    if (request->requestedFormatId == CF_UNICODETEXT && bridge->clipboard_text)
        wide = ConvertUtf8ToWCharAlloc(bridge->clipboard_text, &chars);
    else if (request->requestedFormatId == 0xC001 && bridge->clipboard_files) {
        FILEDESCRIPTORW* descriptors = calloc(bridge->clipboard_file_count, sizeof(*descriptors));
        if (descriptors) {
            BOOL valid = TRUE;
            for (UINT32 i = 0; i < bridge->clipboard_file_count; i++) {
                FILEDESCRIPTORW* d = &descriptors[i];
                d->dwFlags = FD_ATTRIBUTES | FD_FILESIZE | FD_UNICODE;
                d->dwFileAttributes = bridge->clipboard_files[i].directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
                d->nFileSizeHigh = (UINT32)(bridge->clipboard_files[i].size >> 32);
                d->nFileSizeLow = (UINT32)bridge->clipboard_files[i].size;
                size_t count = 0;
                WCHAR* name = ConvertUtf8ToWCharAlloc(bridge->clipboard_files[i].remote_name, &count);
                if (!name || count > 259) valid = FALSE;
                else memcpy(d->cFileName, name, (count + 1) * sizeof(WCHAR));
                free(name);
                if (!valid) break;
            }
            if (valid && cliprdr_serialize_file_list(descriptors, bridge->clipboard_file_count,
                                                     &file_data, &file_length) != CHANNEL_RC_OK) file_data = NULL;
            free(descriptors);
        }
    }
    LeaveCriticalSection(&bridge->clipboard_lock);
    if (file_data) {
        response.common.msgFlags = CB_RESPONSE_OK;
        response.common.dataLen = file_length;
        response.requestedFormatData = file_data;
    } else if (wide && chars < 1024 * 1024) {
        response.common.msgFlags = CB_RESPONSE_OK;
        response.common.dataLen = (UINT32)((chars + 1) * sizeof(WCHAR));
        response.requestedFormatData = (const BYTE*)wide;
    } else response.common.msgFlags = CB_RESPONSE_FAIL;
    UINT result = context->ClientFormatDataResponse(context, &response);
    free(wide);
    free(file_data);
    return result;
}
static UINT clipboard_file_request(CliprdrClientContext* context, const CLIPRDR_FILE_CONTENTS_REQUEST* request) {
    RdpBridge* bridge = clipboard_bridge(context);
    CLIPRDR_FILE_CONTENTS_RESPONSE response = { .common = { .msgType = CB_FILECONTENTS_RESPONSE,
        .msgFlags = CB_RESPONSE_FAIL }, .streamId = request->streamId };
    BYTE* data = NULL;
    EnterCriticalSection(&bridge->clipboard_lock);
    if (bridge->clipboard_files && request->listIndex < bridge->clipboard_file_count) {
        const RdpClipboardFile* file = &bridge->clipboard_files[request->listIndex];
        if (request->dwFlags == FILECONTENTS_SIZE) {
            data = malloc(8);
            if (data) { for (int i = 0; i < 8; i++) data[i] = (BYTE)(file->size >> (8 * i));
                response.common.msgFlags = CB_RESPONSE_OK; response.cbRequested = 8; }
        } else if (request->dwFlags == FILECONTENTS_RANGE && !file->directory &&
                   request->cbRequested <= 1024 * 1024) {
            UINT64 offset = ((UINT64)request->nPositionHigh << 32) | request->nPositionLow;
            if (offset <= file->size) {
                size_t length = request->cbRequested;
                if (length > file->size - offset) length = (size_t)(file->size - offset);
                data = malloc(length ? length : 1);
                int fd = open(file->source, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
                struct stat statbuf;
                if (data && fd >= 0 && fstat(fd, &statbuf) == 0 && S_ISREG(statbuf.st_mode) &&
                    (UINT64)statbuf.st_size == file->size && offset <= (UINT64)INT64_MAX &&
                    pread(fd, data, length, (off_t)offset) == (ssize_t)length) {
                    response.common.msgFlags = CB_RESPONSE_OK;
                    response.cbRequested = (UINT32)length;
                }
                if (fd >= 0) close(fd);
            }
        }
    }
    LeaveCriticalSection(&bridge->clipboard_lock);
    response.requestedData = data;
    UINT result = context->ClientFileContentsResponse(context, &response);
    free(data);
    return result;
}
static void clipboard_clear_files(RdpBridge* bridge) {
    for (UINT32 i = 0; i < bridge->clipboard_file_count; i++) {
        free((char*)bridge->clipboard_files[i].source);
        free((char*)bridge->clipboard_files[i].remote_name);
    }
    free(bridge->clipboard_files);
    bridge->clipboard_files = NULL;
    bridge->clipboard_file_count = 0;
}
int rdp_manager_clipboard_set_text(RdpBridge* bridge, const char* text) {
    if (!bridge || !text || strlen(text) > 1024 * 1024) return 0;
    char* copy = strdup(text);
    if (!copy) return 0;
    EnterCriticalSection(&bridge->clipboard_lock);
    clipboard_clear_files(bridge);
    if (bridge->clipboard_text) { SecureZeroMemory(bridge->clipboard_text, strlen(bridge->clipboard_text)); free(bridge->clipboard_text); }
    bridge->clipboard_text = copy;
    LeaveCriticalSection(&bridge->clipboard_lock);
    return clipboard_advertise(bridge) == CHANNEL_RC_OK;
}
int rdp_manager_clipboard_set_files(RdpBridge* bridge, const RdpClipboardFile* files, UINT32 count) {
    if (!bridge || !files || !count || count > 4096) return 0;
    RdpClipboardFile* copy = calloc(count, sizeof(*copy));
    if (!copy) return 0;
    for (UINT32 i = 0; i < count; i++) {
        copy[i] = files[i];
        copy[i].source = files[i].source ? strdup(files[i].source) : NULL;
        copy[i].remote_name = files[i].remote_name ? strdup(files[i].remote_name) : NULL;
        if (!copy[i].source || !copy[i].remote_name) {
            for (UINT32 j = 0; j <= i; j++) { free((char*)copy[j].source); free((char*)copy[j].remote_name); }
            free(copy); return 0;
        }
    }
    EnterCriticalSection(&bridge->clipboard_lock);
    clipboard_clear_files(bridge);
    if (bridge->clipboard_text) { SecureZeroMemory(bridge->clipboard_text, strlen(bridge->clipboard_text));
        free(bridge->clipboard_text); bridge->clipboard_text = NULL; }
    bridge->clipboard_files = copy;
    bridge->clipboard_file_count = count;
    LeaveCriticalSection(&bridge->clipboard_lock);
    return clipboard_advertise(bridge) == CHANNEL_RC_OK;
}
static void channel_connected(void* context, const ChannelConnectedEventArgs* event) {
    rdpContext* ctx = context;
    if (strcmp(event->name, CLIPRDR_SVC_CHANNEL_NAME) == 0) {
        RdpBridge* bridge = bridge_of(ctx);
        CliprdrClientContext* clip = event->pInterface;
        bridge->clipboard = clip;
        bridge->clipboard_ready = FALSE;
        clip->custom = bridge;
        if (bridge->files_callback && access("/dev/fuse", R_OK | W_OK) == 0 &&
            !atomic_exchange(&clipboard_mount_in_use, TRUE)) {
            CliprdrFileContext* files = cliprdr_file_context_new(bridge);
            if (files && cliprdr_file_context_has_local_support(files))
                bridge->remote_clipboard = ClipboardCreate();
            if (bridge->remote_clipboard && cliprdr_file_context_init(files, clip)) {
                bridge->remote_files = files;
                clip->ServerCapabilities = clipboard_capabilities;
            } else if (files) {
                cliprdr_file_context_free(files);
                clip->custom = bridge;
                clip->ServerFileContentsResponse = NULL;
            }
            if (!bridge->remote_files) atomic_store(&clipboard_mount_in_use, FALSE);
            /* Local offers use the bridge's own reader and do not negotiate locking. */
            clip->ServerLockClipboardData = NULL;
            clip->ServerUnlockClipboardData = NULL;
        }
        if (bridge->files_callback)
            clipboard_trace(bridge->remote_files ? "FUSE receiver initialized" :
                "FUSE receiver unavailable (check /dev/fuse, FreeRDP build and concurrent sessions)");
        clip->MonitorReady = clipboard_ready;
        clip->ServerFormatList = clipboard_server_list;
        clip->ServerFormatDataRequest = clipboard_server_request;
        clip->ServerFormatDataResponse = clipboard_server_data;
        clip->ServerFileContentsRequest = clipboard_file_request;
        return;
    }
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
    if (strcmp(event->name, CLIPRDR_SVC_CHANNEL_NAME) == 0) {
        if (bridge->remote_files) {
            BOOL detached = cliprdr_file_context_uninit(bridge->remote_files, bridge->clipboard);
            (void)detached;
            cliprdr_file_context_free(bridge->remote_files);
            bridge->files_callback(bridge->opaque, "");
            bridge->remote_files = NULL;
            atomic_store(&clipboard_mount_in_use, FALSE);
        }
        if (bridge->remote_clipboard) ClipboardDestroy(bridge->remote_clipboard);
        bridge->remote_clipboard = NULL;
        bridge->clipboard = NULL;
        bridge->clipboard_ready = FALSE;
        bridge->requested_format = bridge->offered_format = 0;
    }
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
    const char* audio = getenv("RDP_MANAGER_AUDIO");
    if ((!audio || strcmp(audio, "off") != 0) &&
        strstr(freerdp_get_build_config(), "WITH_PULSE=ON")) {
        unsigned long latency = 150;
        const char* override = getenv("RDP_MANAGER_AUDIO_LATENCY_MS");
        if (override && *override) {
            char* end = NULL;
            unsigned long candidate = strtoul(override, &end, 10);
            if (end != override && *end == '\0' && candidate <= 2000)
                latency = candidate;
        }
        /* Give RDPSND room for packets that arrive in bursts and set the PulseAudio target. */
        char latency_arg[32];
        snprintf(latency_arg, sizeof(latency_arg), "latency:%lu", latency);
        const char* const sound[] = { "rdpsnd", "sys:pulse", latency_arg };
        if (!freerdp_client_add_static_channel(ctx->settings, ARRAYSIZE(sound), sound) ||
            !freerdp_client_add_dynamic_channel(ctx->settings, ARRAYSIZE(sound), sound)) return FALSE;
    }
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
    if (bridge->clipboard) {
        ChannelDisconnectedEventArgs clipboard_event = { .name = CLIPRDR_SVC_CHANNEL_NAME };
        channel_disconnected(instance->context, &clipboard_event);
    }
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
    const char* password, const char* domain, const char* shared_folder, uint32_t width, uint32_t height,
    uint32_t keyboard_layout, uint32_t quality, void* opaque, RdpFrame frame, RdpCertificate certificate, RdpClipboard clipboard) {
    if (!host || !user || !password || !domain || !shared_folder || !frame || !certificate || !port ||
        !clipboard || !width || !height || width > 8192 || height > 8192 || (uint64_t)width * height > 16777216) return NULL;
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
    InitializeCriticalSection(&bridge->clipboard_lock);
    bridge->opaque = opaque; bridge->frame = frame; bridge->certificate = certificate; bridge->clipboard_callback = clipboard;
    bridge->instance = freerdp_new();
    if (!bridge->instance) { DeleteCriticalSection(&bridge->clipboard_lock); DeleteCriticalSection(&bridge->graphics_lock); free(bridge); return NULL; }
    freerdp* instance = bridge->instance;
    instance->ContextSize = sizeof(BridgeContext);
    if (!freerdp_context_new(instance)) { freerdp_free(instance); DeleteCriticalSection(&bridge->clipboard_lock); DeleteCriticalSection(&bridge->graphics_lock); free(bridge); return NULL; }
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
    if (shared_folder[0]) {
        const char* const params[] = { "drive", "RDP-Share", shared_folder };
        if (!freerdp_client_add_device_channel(settings, 3, params)) goto fail;
    }
    SET_BOOL(FreeRDP_RedirectSmartCards, FALSE);
    SET_BOOL(FreeRDP_RemoteFxCodec, quality != 3);
    SET_BOOL(FreeRDP_NSCodec, quality != 3);
    if (quality == 3) {
        SET_BOOL(FreeRDP_RemoteFxImageCodec, FALSE);
        SET_BOOL(FreeRDP_JpegCodec, FALSE);
        SET_UINT(FreeRDP_BitmapCacheV3CodecId, 0);
    }
    SET_BOOL(FreeRDP_RedirectClipboard, TRUE);
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
    if (bridge->clipboard_text) { SecureZeroMemory(bridge->clipboard_text, strlen(bridge->clipboard_text)); free(bridge->clipboard_text); }
    clipboard_clear_files(bridge);
    DeleteCriticalSection(&bridge->clipboard_lock);
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
