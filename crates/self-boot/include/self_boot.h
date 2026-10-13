#ifndef IPU_SELF_BOOT_H
#define IPU_SELF_BOOT_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef struct BootHandle ipu_boot_handle;
/* All calls are blocking. Serialize calls on a handle; do not call from a Tokio runtime.
 * Strings are borrowed NUL-terminated UTF-8. addr is IP:port (IPv6: [IP]:port).
 * Returns NULL on failure. Existing pairing record is verified, never re-paired.
 * The owned runtime keeps heartbeat and DTX alive between calls. */
ipu_boot_handle *ipu_boot_connect_rp(const char *pairing_file, const char *addr);
/* Outer deadline; each connection/protocol stage also has a 30 s deadline. */
ipu_boot_handle *ipu_boot_connect_rp_timeout(const char *pairing_file, const char *addr, uint32_t timeout_secs);
/* iface_name (name or positive index) and local_ip (IPv4) may both be NULL for
 * normal routing; otherwise supply both. Binds BOTH pairing and tunnel sockets. */
ipu_boot_handle *ipu_boot_connect_rp_bound(const char *pairing_file, const char *addr,
    const char *iface_name, const char *local_ip, uint32_t timeout_secs);
/* JSON array of {name,index,address,flags:[UP,RUNNING,POINTOPOINT]} for all
 * IPv4/IPv6 addresses. NULL on error. Same thread-local JSON borrowing rule. */
const char *ipu_boot_list_interfaces_json(void);
/* Borrowed JSON: status + drained DTX/heartbeat events; NULL on error.
 * State is Connected / KeepaliveDegraded(reason) / Disconnected(reason) / Closed.
 * Serialize access; copy result before next JSON-producing ABI call on this thread. */
const char *ipu_boot_status_json(ipu_boot_handle *);
/* Read-only redacted DDI probe; per-request 30 s deadline, partial errors in JSON.
 * Same thread-local borrowing rule as status_json; NULL on ABI error. */
const char *ipu_boot_ddi_probe_json(ipu_boot_handle *);
/* Fresh HTTPS ticket only; no image state changes. Store directory must exist.
 * Redacted JSON, same borrowing convention; NULL on ABI error. */
const char *ipu_boot_ddi_ticket_json(ipu_boot_handle *, const char *ddi_dir, const char *store_dir);
/* Single-attempt personalized mount; skips existing developer images.
 * Redacted phases JSON; same borrowing convention; NULL on ABI error. */
const char *ipu_boot_ddi_mount_json(ipu_boot_handle *, const char *ddi_dir, const char *store_dir);
/* Replies from start_test_sessions / authorize_pid, or status_json. Same borrowing rule. */
const char *ipu_boot_last_json(void);
/* Returns 0 on success, -1 on error; status outputs are 0/1. No image is mounted. */
int32_t ipu_boot_ddi_status(ipu_boot_handle *, int32_t *mounted, int32_t *developer_mode);
int32_t ipu_boot_start_test_sessions(ipu_boot_handle *, const char *session_uuid);
int32_t ipu_boot_authorize_pid(ipu_boot_handle *, uint32_t pid);
/* Thread-local borrowed string; valid until next ABI call on this thread. Do not free. */
const char *ipu_boot_last_error(void);
/* NULL is allowed. Releases DTX, heartbeat, tunnel and runtime; call exactly once. */
void ipu_boot_free(ipu_boot_handle *);
#ifdef __cplusplus
}
#endif
#endif
