// SPDX-License-Identifier: Apache-2.0

//! Bridge to the API cdylib's internal registration ABI.
//!
//! In a normal build these are `extern "C"` imports resolved at load time against
//! `libopentelemetry_c_api` (see `build.rs`). Under `cfg(test)` — where the SDK rlib is
//! linked into a test binary that does *not* load the API cdylib — they are replaced with
//! in-process stubs so the SDK's own unit tests link and can observe registration. The
//! true cross-artifact behavior is proven by the separate C link/run test.

use std::os::raw::{c_char, c_void};

use opentelemetry_c_abi::{OtelImplVtable, OtelLogsVtable, OtelMetricsVtable, OtelStatus};

#[cfg(not(test))]
mod imp {
    use super::*;
    // MSRV is 1.77; `unsafe extern` blocks require 1.82. Keep a plain extern block and
    // allow the Rust-2024-compat lint. These import the API cdylib's internal symbols.
    #[allow(unknown_lints)]
    #[allow(missing_unsafe_on_extern)]
    extern "C" {
        pub fn otel_api_register_global_provider(
            vtable: *const OtelImplVtable,
            provider_ctx: *mut c_void,
        ) -> OtelStatus;
        pub fn otel_api_provider_new(
            vtable: *const OtelImplVtable,
            provider_ctx: *mut c_void,
        ) -> *mut c_void;
        pub fn otel_api_register_global_meter_provider_with_token(
            vtable: *const OtelMetricsVtable,
            provider_ctx: *mut c_void,
            out_id: *mut u64,
        ) -> OtelStatus;
        pub fn otel_api_unregister_global_meter_provider(registration_id: u64) -> OtelStatus;
        pub fn otel_api_meter_provider_new(
            vtable: *const OtelMetricsVtable,
            provider_ctx: *mut c_void,
        ) -> *mut c_void;
        pub fn otel_api_register_global_logger_provider_with_token(
            vtable: *const OtelLogsVtable,
            provider_ctx: *mut c_void,
            out_id: *mut u64,
        ) -> OtelStatus;
        pub fn otel_api_unregister_global_logger_provider(registration_id: u64) -> OtelStatus;
        pub fn otel_api_logger_provider_new(
            vtable: *const OtelLogsVtable,
            provider_ctx: *mut c_void,
        ) -> *mut c_void;
        pub fn otel_api_set_last_error(ptr: *const c_char, len: usize);
        pub fn otel_api_clear_last_error();
        pub fn otel_api_report_diagnostic(
            severity: u32,
            message: *const c_char,
            message_len: usize,
        );
    }
}

#[cfg(test)]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    thread_local! {
        pub(super) static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    // Records the most recently registered (vtable, ctx) so tests can drive it.
    pub(super) static REGISTERED: Mutex<Option<(usize, usize)>> = Mutex::new(None);
    pub(super) static METRICS_REGISTERED: Mutex<Option<(usize, usize, u64)>> = Mutex::new(None);
    pub(super) static LOGS_REGISTERED: Mutex<Option<(usize, usize, u64)>> = Mutex::new(None);
    static NEXT_METRICS_ID: AtomicU64 = AtomicU64::new(1);
    static NEXT_LOGS_ID: AtomicU64 = AtomicU64::new(1);

    /// # Safety
    /// Test stub mirroring the real ABI.
    pub unsafe fn otel_api_register_global_provider(
        vtable: *const OtelImplVtable,
        provider_ctx: *mut c_void,
    ) -> OtelStatus {
        *REGISTERED.lock().unwrap() = Some((vtable as usize, provider_ctx as usize));
        OtelStatus::Ok
    }
    /// # Safety
    /// Test stub mirroring the real ABI.
    pub unsafe fn otel_api_provider_new(
        _vtable: *const OtelImplVtable,
        provider_ctx: *mut c_void,
    ) -> *mut c_void {
        provider_ctx
    }
    pub unsafe fn otel_api_register_global_meter_provider_with_token(
        vtable: *const OtelMetricsVtable,
        provider_ctx: *mut c_void,
        out_id: *mut u64,
    ) -> OtelStatus {
        if out_id.is_null() {
            return OtelStatus::InvalidArgument;
        }
        let id = NEXT_METRICS_ID.fetch_add(1, Ordering::Relaxed);
        let old = METRICS_REGISTERED.lock().unwrap().replace((
            vtable as usize,
            provider_ctx as usize,
            id,
        ));
        if let Some((old_vtable, old_ctx, _)) = old {
            unsafe {
                ((*(old_vtable as *const OtelMetricsVtable)).provider_free)(old_ctx as *mut c_void)
            };
        }
        unsafe { *out_id = id };
        OtelStatus::Ok
    }
    pub unsafe fn otel_api_unregister_global_meter_provider(registration_id: u64) -> OtelStatus {
        let old = {
            let mut registered = METRICS_REGISTERED.lock().unwrap();
            match *registered {
                Some((_, _, id)) if id == registration_id => registered.take(),
                _ => None,
            }
        };
        if let Some((vtable, ctx, _)) = old {
            unsafe { ((*(vtable as *const OtelMetricsVtable)).provider_free)(ctx as *mut c_void) };
        }
        OtelStatus::Ok
    }
    pub unsafe fn otel_api_register_global_logger_provider_with_token(
        vtable: *const OtelLogsVtable,
        provider_ctx: *mut c_void,
        out_id: *mut u64,
    ) -> OtelStatus {
        if out_id.is_null() {
            return OtelStatus::InvalidArgument;
        }
        let id = NEXT_LOGS_ID.fetch_add(1, Ordering::Relaxed);
        let old =
            LOGS_REGISTERED
                .lock()
                .unwrap()
                .replace((vtable as usize, provider_ctx as usize, id));
        if let Some((old_vtable, old_ctx, _)) = old {
            unsafe {
                ((*(old_vtable as *const OtelLogsVtable)).provider_free)(old_ctx as *mut c_void)
            };
        }
        unsafe { *out_id = id };
        OtelStatus::Ok
    }
    pub unsafe fn otel_api_unregister_global_logger_provider(registration_id: u64) -> OtelStatus {
        let old = {
            let mut registered = LOGS_REGISTERED.lock().unwrap();
            match *registered {
                Some((_, _, id)) if id == registration_id => registered.take(),
                _ => None,
            }
        };
        if let Some((vtable, ctx, _)) = old {
            unsafe { ((*(vtable as *const OtelLogsVtable)).provider_free)(ctx as *mut c_void) };
        }
        OtelStatus::Ok
    }
    pub unsafe fn otel_api_logger_provider_new(
        _vtable: *const OtelLogsVtable,
        provider_ctx: *mut c_void,
    ) -> *mut c_void {
        provider_ctx
    }
    pub unsafe fn otel_api_meter_provider_new(
        _vtable: *const OtelMetricsVtable,
        provider_ctx: *mut c_void,
    ) -> *mut c_void {
        provider_ctx
    }
    /// # Safety
    /// Test stub mirroring the real ABI.
    pub unsafe fn otel_api_set_last_error(ptr: *const c_char, len: usize) {
        LAST_ERROR.with(|slot| {
            let mut b = slot.borrow_mut();
            b.clear();
            if !ptr.is_null() && len > 0 && len <= isize::MAX as usize {
                b.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) });
            }
        });
    }
    /// # Safety
    /// Test stub mirroring the real ABI.
    pub unsafe fn otel_api_clear_last_error() {
        LAST_ERROR.with(|slot| slot.borrow_mut().clear());
    }
    /// # Safety
    /// Test stub mirroring the real ABI. Unit tests do not install the API callback.
    pub unsafe fn otel_api_report_diagnostic(
        _severity: u32,
        _message: *const c_char,
        _message_len: usize,
    ) {
    }
}

/// Install `vtable`/`provider_ctx` as the process-global provider (API-owned slot).
pub(crate) fn register_global_provider(
    vtable: *const OtelImplVtable,
    provider_ctx: *mut c_void,
) -> OtelStatus {
    unsafe { imp::otel_api_register_global_provider(vtable, provider_ctx) }
}

/// Wrap an SDK provider context in an owned API `otel_tracer_provider_t` handle.
pub(crate) fn provider_new(
    vtable: *const OtelImplVtable,
    provider_ctx: *mut c_void,
) -> *mut c_void {
    unsafe { imp::otel_api_provider_new(vtable, provider_ctx) }
}

pub(crate) fn register_global_meter_provider(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
) -> (OtelStatus, u64) {
    let mut registration_id = 0;
    let status = unsafe {
        imp::otel_api_register_global_meter_provider_with_token(
            vtable,
            provider_ctx,
            &mut registration_id,
        )
    };
    (status, registration_id)
}

pub(crate) fn unregister_global_meter_provider(registration_id: u64) -> OtelStatus {
    unsafe { imp::otel_api_unregister_global_meter_provider(registration_id) }
}

pub(crate) fn meter_provider_new(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
) -> *mut c_void {
    unsafe { imp::otel_api_meter_provider_new(vtable, provider_ctx) }
}

pub(crate) fn register_global_logger_provider(
    vtable: *const OtelLogsVtable,
    provider_ctx: *mut c_void,
) -> (OtelStatus, u64) {
    let mut registration_id = 0;
    let status = unsafe {
        imp::otel_api_register_global_logger_provider_with_token(
            vtable,
            provider_ctx,
            &mut registration_id,
        )
    };
    (status, registration_id)
}

pub(crate) fn unregister_global_logger_provider(registration_id: u64) -> OtelStatus {
    unsafe { imp::otel_api_unregister_global_logger_provider(registration_id) }
}

pub(crate) fn logger_provider_new(
    vtable: *const OtelLogsVtable,
    provider_ctx: *mut c_void,
) -> *mut c_void {
    unsafe { imp::otel_api_logger_provider_new(vtable, provider_ctx) }
}

/// Record a diagnostic in the API-owned thread-local error slot.
pub(crate) fn set_last_error(message: &str) {
    unsafe { imp::otel_api_set_last_error(message.as_ptr().cast::<c_char>(), message.len()) };
}

/// Clear the API-owned thread-local error slot.
pub(crate) fn clear_last_error() {
    unsafe { imp::otel_api_clear_last_error() };
}

/// Report an asynchronous or advisory diagnostic through the API-owned callback.
pub(crate) fn report_diagnostic(severity: u32, message: &str) {
    unsafe { imp::otel_api_report_diagnostic(severity, message.as_ptr().cast(), message.len()) }
}

#[cfg(test)]
pub(crate) mod test_probe {
    use super::*;

    pub static METRICS_GLOBAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Only the OTLP-backed `set_as_global` unit test drives this probe; without `otlp-http`
    // feature that test is compiled out, so gate the helper to match (avoids dead_code).
    #[cfg(feature = "otlp-http")]
    pub fn registered() -> Option<(*const OtelImplVtable, *mut c_void)> {
        imp::REGISTERED
            .lock()
            .unwrap()
            .as_ref()
            .map(|&(v, c)| (v as *const OtelImplVtable, c as *mut c_void))
    }

    pub static LOGS_GLOBAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub fn logs_registered() -> bool {
        imp::LOGS_REGISTERED.lock().unwrap().is_some()
    }

    pub fn logs_registration_id() -> Option<u64> {
        imp::LOGS_REGISTERED
            .lock()
            .unwrap()
            .as_ref()
            .map(|&(_, _, id)| id)
    }

    pub fn metrics_registered() -> bool {
        imp::METRICS_REGISTERED.lock().unwrap().is_some()
    }

    pub fn metrics_registration_id() -> Option<u64> {
        imp::METRICS_REGISTERED
            .lock()
            .unwrap()
            .as_ref()
            .map(|&(_, _, id)| id)
    }

    /// The current thread's recorded last-error message (empty if none).
    pub fn last_error() -> String {
        imp::LAST_ERROR.with(|slot| String::from_utf8_lossy(&slot.borrow()).into_owned())
    }
}
