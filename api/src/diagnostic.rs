// SPDX-License-Identifier: Apache-2.0

//! API-owned process diagnostic callback and the SDK-to-API reporting bridge.

use std::cell::Cell;
use std::os::raw::{c_char, c_void};
use std::sync::{Arc, RwLock};

use opentelemetry_c_abi::{OtelStatus, OtelStringView};

use crate::error::{clear_last_error, fail};

const OTEL_DIAGNOSTIC_SEVERITY_INFO: u32 = 1;
const OTEL_DIAGNOSTIC_SEVERITY_ERROR: u32 = 3;

#[repr(C)]
pub struct OtelDiagnosticRecord {
    pub struct_size: usize,
    pub severity: u32,
    pub reserved: u32,
    pub message: OtelStringView,
}

pub type DiagnosticEmit = extern "C" fn(*mut c_void, *const OtelDiagnosticRecord);
pub type DiagnosticStateDestroy = extern "C" fn(*mut c_void);

#[repr(C)]
pub struct OtelDiagnosticCallbacks {
    pub struct_size: usize,
    pub emit: Option<DiagnosticEmit>,
    pub state_destroy: Option<DiagnosticStateDestroy>,
}

const REQUIRED_SIZE: usize = std::mem::offset_of!(OtelDiagnosticCallbacks, emit)
    + std::mem::size_of::<Option<DiagnosticEmit>>();
const DESTROY_SIZE: usize = std::mem::offset_of!(OtelDiagnosticCallbacks, state_destroy)
    + std::mem::size_of::<Option<DiagnosticStateDestroy>>();

struct DiagnosticSink {
    emit: DiagnosticEmit,
    state_destroy: Option<DiagnosticStateDestroy>,
    user_data: *mut c_void,
}

// SAFETY: the public contract requires callback state to support concurrent invocation.
unsafe impl Send for DiagnosticSink {}
unsafe impl Sync for DiagnosticSink {}

impl Drop for DiagnosticSink {
    fn drop(&mut self) {
        if let Some(destroy) = self.state_destroy {
            destroy(self.user_data);
        }
    }
}

static DIAGNOSTIC_SINK: RwLock<Option<Arc<DiagnosticSink>>> = RwLock::new(None);

thread_local! {
    static REPORTING_DIAGNOSTIC: Cell<bool> = const { Cell::new(false) };
}

struct ReportingGuard;

impl Drop for ReportingGuard {
    fn drop(&mut self) {
        REPORTING_DIAGNOSTIC.with(|reporting| reporting.set(false));
    }
}

fn valid_severity(severity: u32) -> bool {
    matches!(
        severity,
        OTEL_DIAGNOSTIC_SEVERITY_INFO..=OTEL_DIAGNOSTIC_SEVERITY_ERROR
    )
}

fn report(severity: u32, message: &[u8]) {
    if !valid_severity(severity) {
        return;
    }
    // Set the flag without retaining a TLS borrow. The global lock and foreign callback are
    // reached only after that borrow ends. A callback may re-enter OpenTelemetry C, but a
    // diagnostic produced by that nested call is suppressed on this thread.
    if REPORTING_DIAGNOSTIC.with(|reporting| reporting.replace(true)) {
        return;
    }
    let _reporting_guard = ReportingGuard;
    let sink = DIAGNOSTIC_SINK
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let Some(sink) = sink else { return };
    let record = OtelDiagnosticRecord {
        struct_size: std::mem::size_of::<OtelDiagnosticRecord>(),
        severity,
        reserved: 0,
        message: OtelStringView {
            ptr: message.as_ptr().cast::<c_char>(),
            len: message.len(),
        },
    };
    (sink.emit)(sink.user_data, &record);
}

/// Install, replace, or clear the process-wide diagnostic callback.
///
/// # Safety
/// `callbacks` must be NULL or point to its advertised `struct_size` readable bytes. The
/// callback table and `user_data` must satisfy the public lifetime and concurrency contract.
#[no_mangle]
pub unsafe extern "C" fn otel_set_diagnostic_callback(
    callbacks: *const OtelDiagnosticCallbacks,
    user_data: *mut c_void,
) -> OtelStatus {
    crate::handle::guard_status(|| {
        clear_last_error();
        let next = if callbacks.is_null() {
            None
        } else {
            // Read the prefix without forming a reference to a possibly truncated table.
            let struct_size = unsafe { callbacks.cast::<usize>().read_unaligned() };
            if struct_size < REQUIRED_SIZE {
                return fail(
                    OtelStatus::InvalidArgument,
                    "diagnostic callback struct_size is smaller than the required prefix",
                );
            }
            let emit = unsafe {
                callbacks
                    .cast::<u8>()
                    .add(std::mem::offset_of!(OtelDiagnosticCallbacks, emit))
                    .cast::<Option<DiagnosticEmit>>()
                    .read_unaligned()
            };
            let Some(emit) = emit else {
                return fail(
                    OtelStatus::InvalidArgument,
                    "diagnostic emit callback must not be NULL",
                );
            };
            let state_destroy = if struct_size >= DESTROY_SIZE {
                unsafe {
                    callbacks
                        .cast::<u8>()
                        .add(std::mem::offset_of!(OtelDiagnosticCallbacks, state_destroy))
                        .cast::<Option<DiagnosticStateDestroy>>()
                        .read_unaligned()
                }
            } else {
                None
            };
            Some(Arc::new(DiagnosticSink {
                emit,
                state_destroy,
                user_data,
            }))
        };
        let old = {
            let mut current = DIAGNOSTIC_SINK.write().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut *current, next)
        };
        drop(old);
        OtelStatus::Ok
    })
}

/// Internal ABI used by the SDK for asynchronous and advisory diagnostics.
///
/// # Safety
/// `message` must point to `message_len` readable bytes for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn otel_api_report_diagnostic(
    severity: u32,
    message: *const c_char,
    message_len: usize,
) {
    crate::handle::guard_unit(|| {
        if message.is_null() || message_len == 0 || message_len > isize::MAX as usize {
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(message.cast::<u8>(), message_len) };
        report(severity, bytes);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static EMITS: AtomicUsize = AtomicUsize::new(0);
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn emit(_state: *mut c_void, record: *const OtelDiagnosticRecord) {
        assert!(!record.is_null());
        EMITS.fetch_add(1, Ordering::Relaxed);
    }

    extern "C" fn destroy(_state: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::Relaxed);
    }

    extern "C" fn recursive_emit(_state: *mut c_void, _record: *const OtelDiagnosticRecord) {
        EMITS.fetch_add(1, Ordering::Relaxed);
        let nested = b"nested diagnostic";
        unsafe {
            otel_api_report_diagnostic(
                OTEL_DIAGNOSTIC_SEVERITY_ERROR,
                nested.as_ptr().cast(),
                nested.len(),
            )
        };
    }

    #[test]
    fn callback_reports_and_replacement_destroys_state() {
        EMITS.store(0, Ordering::Relaxed);
        DESTROYS.store(0, Ordering::Relaxed);
        let callbacks = OtelDiagnosticCallbacks {
            struct_size: std::mem::size_of::<OtelDiagnosticCallbacks>(),
            emit: Some(emit),
            state_destroy: Some(destroy),
        };
        assert_eq!(
            unsafe { otel_set_diagnostic_callback(&callbacks, std::ptr::null_mut()) },
            OtelStatus::Ok
        );
        let message = b"background export failed";
        unsafe {
            otel_api_report_diagnostic(
                OTEL_DIAGNOSTIC_SEVERITY_ERROR,
                message.as_ptr().cast(),
                message.len(),
            )
        };
        assert_eq!(EMITS.load(Ordering::Relaxed), 1);
        assert_eq!(
            unsafe { otel_set_diagnostic_callback(std::ptr::null(), std::ptr::null_mut()) },
            OtelStatus::Ok
        );
        assert_eq!(DESTROYS.load(Ordering::Relaxed), 1);

        let recursive = OtelDiagnosticCallbacks {
            struct_size: std::mem::size_of::<OtelDiagnosticCallbacks>(),
            emit: Some(recursive_emit),
            state_destroy: None,
        };
        assert_eq!(
            unsafe { otel_set_diagnostic_callback(&recursive, std::ptr::null_mut()) },
            OtelStatus::Ok
        );
        unsafe {
            otel_api_report_diagnostic(
                OTEL_DIAGNOSTIC_SEVERITY_ERROR,
                message.as_ptr().cast(),
                message.len(),
            )
        };
        assert_eq!(EMITS.load(Ordering::Relaxed), 2);
        assert_eq!(
            unsafe { otel_set_diagnostic_callback(std::ptr::null(), std::ptr::null_mut()) },
            OtelStatus::Ok
        );
    }
}
