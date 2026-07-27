//! C callback-backed Logs exporter.
//!
//! The pinned `LogExporter::export` returns a return-position `impl Future`, so the trait is
//! not object-safe. That is not a problem here: exactly like [`crate::custom_metric_exporter`]
//! this is a **concrete** wrapper stored in the internal [`LogExporterImpl`] enum, so both the
//! simple and the batch log processor consume it without any public API change. The C callback
//! is invoked synchronously and the wrapper returns an already-ready future.
//!
//! ## Callback-state ownership
//!
//! * `user_data` stays caller-owned until `otel_custom_log_exporter_new` returns
//!   `OTEL_STATUS_OK`; on every failure path `state_destroy` is not invoked.
//! * After a successful transfer the exporter owns `user_data` and runs `state_destroy`
//!   exactly once, from `Drop`, after every in-flight callback has returned.
//!
//! ## Why shutdown takes the write lock
//!
//! `shutdown` is the only operation that mutates the shutdown flag, and it does so under a
//! write lock while every export holds the *read* lock for the whole callback. That is what
//! makes "state_destroy runs only after every in-flight callback completed" a mechanical
//! property rather than a comment: `Drop` calls `shutdown`, which cannot acquire the write
//! lock until the last exporting thread has released its read lock.
//!
//! ## Reentrancy
//!
//! Both pinned processors export inside `Context::enter_telemetry_suppressed_scope()`, so a
//! log record emitted from inside the export callback is dropped by the SDK logger instead of
//! recursing. Callbacks must still not shut down or destroy the SDK, provider, processor, or
//! exporter that is currently invoking them; the simple processor holds its exporter mutex
//! across the callback and this wrapper holds its shutdown read lock, so a reentrant shutdown
//! would self-deadlock.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::RwLock;
use std::time::Duration;

use opentelemetry_c_abi::OtelStatus;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::LogBatch;
use opentelemetry_sdk::Resource;

use crate::error::{clear_last_error, fail, fail_owned};
use crate::handle::{guard_status, into_raw};
use crate::log_export_view::{convert_batch, OtelLogExportBatchView};
use crate::log_exporter::{LogExporterImpl, OtelLogExporter};

/// Export one borrowed batch. Every pointer reachable from the batch dies with the call.
pub type OtelCustomLogExport =
    Option<extern "C" fn(*mut c_void, *const OtelLogExportBatchView) -> OtelStatus>;
/// Shut the callback state down. Invoked at most once with a millisecond budget.
pub type OtelCustomLogShutdown = Option<extern "C" fn(*mut c_void, u64) -> OtelStatus>;
/// Release the callback state. Invoked exactly once after a successful transfer.
pub type OtelCustomLogStateDestroy = Option<extern "C" fn(*mut c_void)>;

/// Callback table accepted by [`otel_custom_log_exporter_new`].
///
/// There is deliberately no force-flush member: the pinned `LogExporter` trait has no
/// force-flush operation, so upstream would never invoke one.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelCustomLogExporterCallbacks {
    /// `sizeof(otel_custom_log_exporter_callbacks_t)` as compiled by the caller.
    pub struct_size: usize,
    /// Required.
    pub export_logs: OtelCustomLogExport,
    /// Optional; treated as a successful no-op when NULL.
    pub shutdown: OtelCustomLogShutdown,
    /// Optional; the callback state is simply not released when NULL.
    pub state_destroy: OtelCustomLogStateDestroy,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelCustomLogExporterCallbacks>() == 32);
    assert!(std::mem::align_of::<OtelCustomLogExporterCallbacks>() == 8);
    // The validated prefix must start at offset zero for the short-table check to be sound.
    assert!(std::mem::offset_of!(OtelCustomLogExporterCallbacks, struct_size) == 0);
    assert!(std::mem::offset_of!(OtelCustomLogExporterCallbacks, export_logs) == 8);
    assert!(std::mem::offset_of!(OtelCustomLogExporterCallbacks, shutdown) == 16);
    assert!(std::mem::offset_of!(OtelCustomLogExporterCallbacks, state_destroy) == 24);
};

pub(crate) struct CustomLogExporter {
    callbacks: OtelCustomLogExporterCallbacks,
    user_data: *mut c_void,
    /// Delivered by `set_resource`, which the pinned trait declares `&mut self`, so no lock is
    /// needed and none is ever held across the C callback.
    resource: Resource,
    shutdown: RwLock<bool>,
}

// SAFETY: the wrapper only ever hands `user_data` back to the C callbacks it was registered
// with. The documented contract requires that callback state be safe to use from the SDK
// thread that drives the configured processor.
unsafe impl Send for CustomLogExporter {}
unsafe impl Sync for CustomLogExporter {}

impl std::fmt::Debug for CustomLogExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CustomLogExporter")
    }
}

impl CustomLogExporter {
    /// Invoke one C callback with panic containment and map its status onto `OTelSdkResult`.
    ///
    /// Statuses that cannot describe the outcome of an export or shutdown are rejected with a
    /// distinct diagnostic rather than being folded into one generic failure.
    fn callback_result(
        &self,
        operation: &str,
        timeout: Duration,
        callback: impl FnOnce() -> OtelStatus,
    ) -> OTelSdkResult {
        let status = catch_unwind(AssertUnwindSafe(callback)).map_err(|_| {
            OTelSdkError::InternalFailure(format!(
                "custom log exporter {operation} callback panicked"
            ))
        })?;
        match status {
            OtelStatus::Ok => Ok(()),
            OtelStatus::AlreadyShutdown => Err(OTelSdkError::AlreadyShutdown),
            OtelStatus::Timeout => Err(OTelSdkError::Timeout(timeout)),
            OtelStatus::ExportFailed | OtelStatus::InternalError => {
                Err(OTelSdkError::InternalFailure(format!(
                    "custom log exporter {operation} callback failed with status {}",
                    status.0
                )))
            }
            status => Err(OTelSdkError::InternalFailure(format!(
                "custom log exporter {operation} callback returned status {}, which is not a \
                 valid result for this callback",
                status.0
            ))),
        }
    }

    pub(crate) fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        // Held for the whole callback so a concurrent shutdown cannot release the callback
        // state underneath an in-flight export.
        let shutdown = self
            .shutdown
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        let Some(callback) = self.callbacks.export_logs else {
            return Err(OTelSdkError::InternalFailure(
                "custom log exporter lost its validated export callback".to_owned(),
            ));
        };
        // Conversion happens before the callback runs, so the callback never observes a
        // partially converted batch.
        let storage = match convert_batch(&batch, &self.resource) {
            Ok(storage) => storage,
            Err(error) => {
                // Useful on the emitting thread with a simple processor; on the batch worker
                // thread it is recorded for completeness only.
                fail_owned(error.status, error.message.clone());
                return Err(OTelSdkError::InternalFailure(error.message));
            }
        };
        let view = storage.view();
        let result =
            self.callback_result("export", Duration::ZERO, || callback(self.user_data, view));
        // Explicit so it is obvious that every borrowed pointer dies with the callback.
        drop(storage);
        result
    }

    pub(crate) fn shutdown(&self, timeout: Duration) -> OTelSdkResult {
        let mut shutdown = self
            .shutdown
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        *shutdown = true;
        match self.callbacks.shutdown {
            Some(callback) => self.callback_result("shutdown", timeout, || {
                callback(
                    self.user_data,
                    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                )
            }),
            None => Ok(()),
        }
    }

    pub(crate) fn set_resource(&mut self, resource: &Resource) {
        self.resource = resource.clone();
    }
}

impl Drop for CustomLogExporter {
    fn drop(&mut self) {
        // Idempotent: a processor that already shut the exporter down gets `AlreadyShutdown`
        // here, which is exactly why the C-visible shutdown callback runs at most once.
        let _ = self.shutdown(Duration::from_secs(5));
        if let Some(destroy) = self.callbacks.state_destroy {
            let _ = catch_unwind(AssertUnwindSafe(|| destroy(self.user_data)));
        }
    }
}

/// Create a Logs exporter backed by C callbacks.
///
/// On `OTEL_STATUS_OK` the exporter owns `user_data` and invokes `state_destroy` exactly once.
/// On every failure the caller still owns `user_data` and `state_destroy` is not invoked.
///
/// # Safety
///
/// `callbacks` must address a readable callback structure whose `struct_size` describes it.
/// `out` must address writable storage. Callback state must remain valid until `state_destroy`
/// is invoked.
#[no_mangle]
pub unsafe extern "C" fn otel_custom_log_exporter_new(
    callbacks: *const OtelCustomLogExporterCallbacks,
    user_data: *mut c_void,
    out: *mut *mut OtelLogExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        if callbacks.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "custom log exporter callbacks must not be NULL",
            );
        }
        // Read only the leading `struct_size` word until it proves the rest is readable.
        let struct_size = unsafe { std::ptr::read_unaligned(callbacks.cast::<usize>()) };
        if struct_size < std::mem::size_of::<OtelCustomLogExporterCallbacks>() {
            return fail(
                OtelStatus::InvalidConfig,
                "custom log exporter callback structure is smaller than the required ABI size",
            );
        }
        let callbacks = unsafe { &*callbacks };
        if callbacks.export_logs.is_none() {
            return fail(
                OtelStatus::InvalidConfig,
                "custom log exporter requires an export callback",
            );
        }
        let exporter = CustomLogExporter {
            callbacks: *callbacks,
            user_data,
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        };
        unsafe { *out = into_raw(OtelLogExporter::new(LogExporterImpl::Custom(exporter))) };
        OtelStatus::Ok
    })
}
