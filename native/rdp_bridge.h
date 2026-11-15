#ifndef RDP_MANAGER_BRIDGE_H
#define RDP_MANAGER_BRIDGE_H
#include <stdint.h>
#include <stddef.h>
typedef struct RdpBridge RdpBridge;
/* Frame result: 0 error, 1 consumed, 2 busy (retain dirty framebuffer). */
typedef int (*RdpFrame)(void*, const uint8_t*, uint32_t, uint32_t, uint32_t);
typedef int (*RdpCertificate)(void*, int, const char*, uint16_t, const char*, const char*, const char*, const char*, const char*);
int rdp_manager_initialize(void);
const char* rdp_manager_build_config(void);
RdpBridge* rdp_manager_new(const char* host, uint16_t port, const char* user,
                          const char* password, const char* domain, uint32_t width, uint32_t height,
                          void* opaque, RdpFrame frame, RdpCertificate certificate);
int rdp_manager_connect(RdpBridge*);
int rdp_manager_poll(RdpBridge*);
void rdp_manager_graphics_stats(RdpBridge*, char*, size_t);
int rdp_manager_input(RdpBridge*, uint32_t kind, uint16_t flags, uint16_t a, uint16_t b);
/* Thread-safe cancellation; wakes an active poll or aborts a pending connect. */
void rdp_manager_cancel(RdpBridge*);
/* Thread-safe input notification; lifetime protected by the Rust native mutex. */
void rdp_manager_wake(RdpBridge*);
/* Owner only: reset notification BEFORE draining the input queues. */
void rdp_manager_prepare_input(RdpBridge*);
uint32_t rdp_manager_error(RdpBridge*);
const char* rdp_manager_error_name(RdpBridge*);
void rdp_manager_free(RdpBridge*);
#endif
