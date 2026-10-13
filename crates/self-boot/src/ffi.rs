//! Blocking C ABI. Each handle owns a multi-thread runtime so heartbeat continues between calls.
use crate::{Error, Result, Session, TestSessions};
use std::{
    borrow::Borrow,
    cell::RefCell,
    ffi::{CStr, CString, c_char},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
};
thread_local! { static LAST_JSON: RefCell<CString> = RefCell::new(CString::default()); }
thread_local! { static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default()); }
pub struct BootHandle {
    events: tokio::sync::broadcast::Receiver<crate::Event>,
    tests: Option<TestSessions>,
    session: Session,
    runtime: tokio::runtime::Runtime,
}
fn remember(message: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(message.replace('\0', "\\0")).unwrap());
}
fn boundary<T>(fallback: T, f: impl FnOnce() -> Result<T>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => {
            remember(String::new());
            value
        }
        Ok(Err(e)) => {
            remember(e.to_string());
            fallback
        }
        Err(_) => {
            remember("panic in self-boot".into());
            fallback
        }
    }
}
// Construct and poll futures on a dedicated stack, including when creating a runtime.
// Returning the runtime owner lets connect retain it for background tasks between calls.
fn run_ffi_future<R, F, T>(
    runtime: impl FnOnce() -> Result<R> + Send,
    future: impl FnOnce() -> F + Send,
) -> Result<(R, T)>
where
    R: Borrow<tokio::runtime::Runtime> + Send,
    F: Future<Output = Result<T>>,
    T: Send,
{
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("ipu-boot-ffi".into())
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                let runtime = runtime()?;
                let result = runtime.borrow().block_on(Box::pin(future()))?;
                Ok((runtime, result))
            })?
            .join()
            .map_err(|_| Error::InvalidArgument("panic in self-boot FFI worker".into()))?
    })
}
unsafe fn text<'a>(ptr: *const c_char) -> Result<&'a str> {
    if ptr.is_null() {
        return Err(Error::InvalidArgument("null string".into()));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|e| Error::InvalidArgument(e.to_string()))
}
unsafe fn handle<'a>(ptr: *mut BootHandle) -> Result<&'a mut BootHandle> {
    unsafe { ptr.as_mut() }.ok_or_else(|| Error::InvalidArgument("null handle".into()))
}
/// # Safety
/// Inputs must be valid NUL-terminated UTF-8 strings for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_connect_rp(
    file: *const c_char,
    addr: *const c_char,
) -> *mut BootHandle {
    unsafe { ipu_boot_connect_rp_timeout(file, addr, 30) }
}
/// # Safety
/// Same requirements as connect_rp; timeout_secs must be positive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_connect_rp_timeout(
    file: *const c_char,
    addr: *const c_char,
    timeout_secs: u32,
) -> *mut BootHandle {
    unsafe {
        ipu_boot_connect_rp_bound(file, addr, std::ptr::null(), std::ptr::null(), timeout_secs)
    }
}
/// # Safety
/// Required strings must be valid NUL-terminated UTF-8; optional binding strings may
/// both be NULL, or must both be supplied. The interface accepts a name or index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_connect_rp_bound(
    file: *const c_char,
    addr: *const c_char,
    iface_name: *const c_char,
    local_ip: *const c_char,
    timeout_secs: u32,
) -> *mut BootHandle {
    boundary(std::ptr::null_mut(), || {
        let optional = |ptr: *const c_char| {
            if ptr.is_null() {
                Ok(None)
            } else {
                unsafe { text(ptr) }.map(Some)
            }
        };
        let binding = crate::LocalBind::parse(optional(iface_name)?, optional(local_ip)?)?;
        if timeout_secs == 0 {
            return Err(Error::InvalidArgument("zero timeout".into()));
        }
        let file = unsafe { text(file)? };
        let addr = unsafe { text(addr)? }
            .parse()
            .map_err(|e| Error::InvalidArgument(format!("address: {e}")))?;
        let (runtime, session) = run_ffi_future(
            || {
                Ok(tokio::runtime::Builder::new_multi_thread()
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()?)
            },
            || async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(u64::from(timeout_secs)),
                    Box::pin(crate::connect_rp(file, addr, binding)),
                )
                .await
                .map_err(|_| Error::Timeout("C connect deadline"))?
            },
        )?;
        Ok(Box::into_raw(Box::new(BootHandle {
            events: session.subscribe(),
            tests: None,
            session,
            runtime,
        })))
    })
}
/// # Safety
/// Handle must be live and exclusively accessed. Both output pointers must be writable int32s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_ddi_status(
    ptr: *mut BootHandle,
    mounted: *mut i32,
    developer_mode: *mut i32,
) -> i32 {
    boundary(-1, || {
        if mounted.is_null() || developer_mode.is_null() {
            return Err(Error::InvalidArgument("null status output".into()));
        }
        let h = unsafe { handle(ptr)? };
        let (_, status) = run_ffi_future(|| Ok(&h.runtime), || h.session.ddi_status())?;
        unsafe {
            *mounted = i32::from(status.mounted);
            *developer_mode = i32::from(status.developer_mode);
        }
        Ok(0)
    })
}
/// # Safety
/// Handle must be live and exclusively accessed; uuid must be a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_start_test_sessions(
    ptr: *mut BootHandle,
    uuid: *const c_char,
) -> i32 {
    boundary(-1, || {
        let h = unsafe { handle(ptr)? };
        if h.tests.is_some() {
            return Err(Error::InvalidArgument(
                "test sessions already started".into(),
            ));
        }
        let id = uuid::Uuid::parse_str(unsafe { text(uuid)? })
            .map_err(|e| Error::InvalidArgument(e.to_string()))?;
        let (_, tests) = run_ffi_future(|| Ok(&h.runtime), || h.session.start_test_sessions(id))?;
        json(
            serde_json::json!({"control_reply": tests.control_reply, "test_reply": tests.test_reply}),
        );
        h.tests = Some(tests);
        Ok(0)
    })
}
/// # Safety
/// Handle must be live and exclusively accessed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_authorize_pid(ptr: *mut BootHandle, pid: u32) -> i32 {
    boundary(-1, || {
        let h = unsafe { handle(ptr)? };
        let tests = h
            .tests
            .as_ref()
            .ok_or_else(|| Error::InvalidArgument("test sessions not started".into()))?;
        let (_, reply) = run_ffi_future(|| Ok(&h.runtime), || tests.authorize_pid(pid))?;
        json(serde_json::json!({"authorize_reply": reply}));
        Ok(0)
    })
}
fn json(value: serde_json::Value) {
    LAST_JSON.with(|slot| *slot.borrow_mut() = CString::new(value.to_string()).unwrap());
}
/// All IPv4/IPv6 interface addresses. Borrowed until the next JSON-producing call.
#[unsafe(no_mangle)]
pub extern "C" fn ipu_boot_list_interfaces_json() -> *const c_char {
    boundary(std::ptr::null(), || {
        json(
            serde_json::to_value(crate::network::list_interfaces()?)
                .map_err(|e| Error::Protocol(format!("interface JSON: {e}")))?,
        );
        Ok(ipu_boot_last_json())
    })
}
/// JSON from start_test_sessions, authorize_pid, or status_json; borrowed until next such call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn ipu_boot_last_json() -> *const c_char {
    LAST_JSON.with(|slot| slot.borrow().as_ptr())
}
/// # Safety
/// Handle must be live and exclusively accessed. Drains recent events, including Marco/Polo and DTX errors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_status_json(ptr: *mut BootHandle) -> *const c_char {
    boundary(std::ptr::null(), || {
        let h = unsafe { handle(ptr)? };
        let mut events = Vec::new();
        let mut lagged = 0;
        loop {
            match h.events.try_recv() {
                Ok(event) => events.push(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => lagged += n,
                Err(_) => break,
            }
        }
        json(
            serde_json::json!({"state": h.session.state(), "test_sessions": h.tests.is_some(), "events": events, "lagged": lagged}),
        );
        Ok(ipu_boot_last_json())
    })
}
/// Read-only, redacted DDI report. Borrowed until next JSON-producing call on this thread.
/// # Safety
/// Handle must be live and exclusively accessed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_ddi_probe_json(ptr: *mut BootHandle) -> *const c_char {
    boundary(std::ptr::null(), || {
        let h = unsafe { handle(ptr)? };
        let (_, report) = run_ffi_future(|| Ok(&h.runtime), || h.session.ddi_probe())?;
        json(report);
        Ok(ipu_boot_last_json())
    })
}
/// Redacted ticket report; same borrowing convention as ddi_probe_json.
/// # Safety
/// Handle must be live/exclusive; paths must be valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_ddi_ticket_json(
    ptr: *mut BootHandle,
    ddi_dir: *const c_char,
    store_dir: *const c_char,
) -> *const c_char {
    boundary(std::ptr::null(), || {
        let h = unsafe { handle(ptr)? };
        let ddi = std::path::Path::new(unsafe { text(ddi_dir)? });
        let store = std::path::Path::new(unsafe { text(store_dir)? });
        let (_, report) = run_ffi_future(|| Ok(&h.runtime), || h.session.ddi_ticket(ddi, store))?;
        json(report);
        Ok(ipu_boot_last_json())
    })
}
/// Redacted single-attempt DDI mount report; same borrowing convention as ddi_ticket_json.
/// # Safety
/// Handle must be live/exclusive; paths must be valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_ddi_mount_json(
    ptr: *mut BootHandle,
    ddi_dir: *const c_char,
    store_dir: *const c_char,
) -> *const c_char {
    boundary(std::ptr::null(), || {
        let h = unsafe { handle(ptr)? };
        let ddi = std::path::Path::new(unsafe { text(ddi_dir)? });
        let store = std::path::Path::new(unsafe { text(store_dir)? });
        let (_, report) = run_ffi_future(|| Ok(&h.runtime), || h.session.ddi_mount(ddi, store))?;
        json(report);
        Ok(ipu_boot_last_json())
    })
}
/// Thread-local borrowed string, valid until the next ABI operation on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn ipu_boot_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}
/// # Safety
/// Pointer must be null or a live handle returned by connect_rp, freed exactly once; no concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_boot_free(ptr: *mut BootHandle) {
    boundary((), || {
        if !ptr.is_null() {
            let mut h = unsafe { Box::from_raw(ptr) };
            h.tests.take();
            run_ffi_future(|| Ok(&h.runtime), || h.session.close())?;
        }
        Ok(())
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_future_runs_from_small_caller_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let (_, sum) = run_ffi_future(
                    || Ok(tokio::runtime::Builder::new_current_thread().build()?),
                    || async {
                        let bytes = std::hint::black_box([7u8; 1024 * 1024]);
                        tokio::task::yield_now().await;
                        Ok(bytes.iter().map(|&b| u64::from(b)).sum::<u64>())
                    },
                )
                .unwrap();
                assert_eq!(sum, 7 * 1024 * 1024);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn worker_panic_returns_error() {
        let result = run_ffi_future(
            || Ok(tokio::runtime::Builder::new_current_thread().build()?),
            || async {
                panic!("worker panic");
                #[allow(unreachable_code)]
                Ok(())
            },
        );
        assert!(
            matches!(result, Err(Error::InvalidArgument(message)) if message.contains("panic"))
        );
    }

    #[test]
    fn null_abi_arguments_report_error_without_io() {
        assert_eq!(
            unsafe { ipu_boot_authorize_pid(std::ptr::null_mut(), 1) },
            -1
        );
        assert!(
            unsafe { CStr::from_ptr(ipu_boot_last_error()) }
                .to_str()
                .unwrap()
                .contains("null handle")
        );
        assert!(unsafe { ipu_boot_connect_rp(std::ptr::null(), std::ptr::null()) }.is_null());
        unsafe {
            ipu_boot_free(std::ptr::null_mut());
        }
        assert_eq!(
            unsafe { CStr::from_ptr(ipu_boot_last_error()) }.to_bytes(),
            b""
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn interface_abi_json_contains_loopback() {
        let ptr = ipu_boot_list_interfaces_json();
        assert!(!ptr.is_null());
        let entries: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap()).unwrap();
        assert!(
            entries
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["name"] == "lo0")
        );
    }
    #[test]
    fn boundary_catches_panics_and_nul_errors() {
        assert_eq!(boundary(-1, || -> Result<i32> { panic!("test") }), -1);
        remember("a\0b".into());
        assert_eq!(
            unsafe { CStr::from_ptr(ipu_boot_last_error()) }.to_bytes(),
            b"a\\0b"
        );
    }
}
