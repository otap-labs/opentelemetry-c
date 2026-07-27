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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_exporter::otel_log_exporter_destroy;
    use opentelemetry::logs::{
        AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity,
    };
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::logs::{SdkLogRecord, SdkLoggerProvider};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    struct State {
        exports: AtomicUsize,
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
        records_seen: AtomicUsize,
        first_body: AtomicU64,
        export_status: OtelStatus,
        shutdown_status: OtelStatus,
    }

    impl State {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                exports: AtomicUsize::new(0),
                shutdowns: AtomicUsize::new(0),
                destroys: AtomicUsize::new(0),
                records_seen: AtomicUsize::new(0),
                first_body: AtomicU64::new(u64::MAX),
                export_status: OtelStatus::Ok,
                shutdown_status: OtelStatus::Ok,
            })
        }
    }

    extern "C" fn export(
        user_data: *mut c_void,
        batch: *const OtelLogExportBatchView,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<State>()) };
        state.exports.fetch_add(1, Ordering::SeqCst);
        let batch = unsafe { &*batch };
        state
            .records_seen
            .fetch_add(batch.record_count, Ordering::SeqCst);
        if batch.record_count > 0 {
            let record = unsafe { &*batch.records };
            if record.present_fields & crate::log_export_view::OTEL_LOG_EXPORT_FIELD_BODY != 0 {
                state.first_body.store(
                    unsafe { record.body.value.int64_value } as u64,
                    Ordering::SeqCst,
                );
            }
            // Proves the scope pointer is dereferenceable for the whole callback.
            assert!(!record.scope.is_null());
        }
        state.export_status
    }

    extern "C" fn shutdown(user_data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<State>()) };
        state.shutdowns.fetch_add(1, Ordering::SeqCst);
        state.shutdown_status
    }

    extern "C" fn destroy(user_data: *mut c_void) {
        let state = unsafe { Arc::from_raw(user_data.cast::<State>()) };
        state.destroys.fetch_add(1, Ordering::SeqCst);
    }

    fn callbacks() -> OtelCustomLogExporterCallbacks {
        OtelCustomLogExporterCallbacks {
            struct_size: std::mem::size_of::<OtelCustomLogExporterCallbacks>(),
            export_logs: Some(export),
            shutdown: Some(shutdown),
            state_destroy: Some(destroy),
        }
    }

    fn exporter_with(state: &Arc<State>) -> CustomLogExporter {
        CustomLogExporter {
            callbacks: callbacks(),
            user_data: Arc::into_raw(Arc::clone(state)).cast_mut().cast(),
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        }
    }

    fn record(body: i64) -> SdkLogRecord {
        let provider = SdkLoggerProvider::builder().build();
        let mut record = provider.logger("test").create_log_record();
        record.set_body(AnyValue::Int(body));
        record.set_severity_number(Severity::Info);
        record
    }

    fn export_one(exporter: &CustomLogExporter, body: i64) -> OTelSdkResult {
        let record = record(body);
        let scope = opentelemetry::InstrumentationScope::builder("test").build();
        let entries = [(&record, &scope)];
        exporter.export(LogBatch::new(&entries))
    }

    #[test]
    fn construction_rejects_a_null_out_pointer() {
        assert_eq!(
            unsafe {
                otel_custom_log_exporter_new(
                    &callbacks(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            OtelStatus::InvalidArgument
        );
    }

    #[test]
    fn construction_rejects_null_callbacks() {
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_log_exporter_new(std::ptr::null(), std::ptr::null_mut(), &mut exporter)
            },
            OtelStatus::InvalidArgument
        );
        assert!(exporter.is_null());
    }

    #[test]
    fn construction_rejects_a_short_callback_table_without_taking_ownership() {
        let state = State::new();
        let raw = Arc::into_raw(Arc::clone(&state));
        let mut short = callbacks();
        short.struct_size = 0;
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_custom_log_exporter_new(&short, raw.cast_mut().cast(), &mut exporter) },
            OtelStatus::InvalidConfig
        );
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::SeqCst), 0);
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn construction_requires_an_export_callback() {
        let state = State::new();
        let raw = Arc::into_raw(Arc::clone(&state));
        let mut without_export = callbacks();
        without_export.export_logs = None;
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_log_exporter_new(&without_export, raw.cast_mut().cast(), &mut exporter)
            },
            OtelStatus::InvalidConfig
        );
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::SeqCst), 0);
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn destroying_an_untransferred_exporter_shuts_down_and_destroys_once() {
        let state = State::new();
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_log_exporter_new(
                    &callbacks(),
                    Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        assert!(!exporter.is_null());
        unsafe { otel_log_exporter_destroy(exporter) };
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_missing_shutdown_callback_is_a_successful_no_op() {
        let state = State::new();
        let mut without_shutdown = callbacks();
        without_shutdown.shutdown = None;
        let exporter = CustomLogExporter {
            callbacks: without_shutdown,
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        };
        assert!(exporter.shutdown(Duration::from_secs(1)).is_ok());
        drop(exporter);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 0);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn repeated_shutdown_invokes_the_callback_once() {
        let state = State::new();
        let exporter = exporter_with(&state);
        assert!(exporter.shutdown(Duration::from_secs(1)).is_ok());
        assert!(matches!(
            exporter.shutdown(Duration::from_secs(1)),
            Err(OTelSdkError::AlreadyShutdown)
        ));
        drop(exporter);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn export_after_shutdown_is_rejected_without_reaching_the_callback() {
        let state = State::new();
        let exporter = exporter_with(&state);
        assert!(exporter.shutdown(Duration::from_secs(1)).is_ok());
        assert!(matches!(
            export_one(&exporter, 1),
            Err(OTelSdkError::AlreadyShutdown)
        ));
        assert_eq!(state.exports.load(Ordering::SeqCst), 0);
        drop(exporter);
    }

    #[test]
    fn export_delivers_the_converted_batch() {
        let state = State::new();
        let exporter = exporter_with(&state);
        assert!(export_one(&exporter, 42).is_ok());
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.records_seen.load(Ordering::SeqCst), 1);
        assert_eq!(state.first_body.load(Ordering::SeqCst), 42);
        drop(exporter);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_empty_batch_still_reaches_the_callback() {
        let state = State::new();
        let exporter = exporter_with(&state);
        assert!(exporter.export(LogBatch::new(&[])).is_ok());
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.records_seen.load(Ordering::SeqCst), 0);
        drop(exporter);
    }

    #[test]
    fn a_failing_export_callback_fails_the_export() {
        let mut state = State::new();
        Arc::get_mut(&mut state).unwrap().export_status = OtelStatus::ExportFailed;
        let exporter = exporter_with(&state);
        assert!(matches!(
            export_one(&exporter, 1),
            Err(OTelSdkError::InternalFailure(_))
        ));
        drop(exporter);
    }

    #[test]
    fn callback_timeout_and_already_shutdown_statuses_are_preserved() {
        let mut timing_out = State::new();
        Arc::get_mut(&mut timing_out).unwrap().shutdown_status = OtelStatus::Timeout;
        let exporter = exporter_with(&timing_out);
        assert!(matches!(
            exporter.shutdown(Duration::from_millis(17)),
            Err(OTelSdkError::Timeout(timeout)) if timeout == Duration::from_millis(17)
        ));
        drop(exporter);

        let mut already = State::new();
        Arc::get_mut(&mut already).unwrap().export_status = OtelStatus::AlreadyShutdown;
        let exporter = exporter_with(&already);
        assert!(matches!(
            export_one(&exporter, 1),
            Err(OTelSdkError::AlreadyShutdown)
        ));
        drop(exporter);
    }

    #[test]
    fn a_nonsensical_callback_status_is_reported_as_a_contract_violation() {
        let mut state = State::new();
        Arc::get_mut(&mut state).unwrap().export_status = OtelStatus::InvalidUtf8;
        let exporter = exporter_with(&state);
        let error = export_one(&exporter, 1).expect_err("bogus status fails the export");
        match error {
            OTelSdkError::InternalFailure(message) => {
                assert!(message.contains("not a valid result"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        drop(exporter);
    }

    #[test]
    fn set_resource_is_visible_to_the_next_export() {
        extern "C" fn checking_export(
            user_data: *mut c_void,
            batch: *const OtelLogExportBatchView,
        ) -> OtelStatus {
            let state = unsafe { &*(user_data.cast::<State>()) };
            let batch = unsafe { &*batch };
            state
                .records_seen
                .store(batch.resource_attribute_count, Ordering::SeqCst);
            OtelStatus::Ok
        }

        let state = State::new();
        let mut checking = callbacks();
        checking.export_logs = Some(checking_export);
        let mut exporter = CustomLogExporter {
            callbacks: checking,
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        };
        exporter.set_resource(
            &Resource::builder_empty()
                .with_attributes([KeyValue::new("a", 1_i64), KeyValue::new("b", 2_i64)])
                .build(),
        );
        assert!(export_one(&exporter, 1).is_ok());
        assert_eq!(state.records_seen.load(Ordering::SeqCst), 2);
        drop(exporter);
    }

    #[test]
    fn a_conversion_failure_never_reaches_the_callback() {
        let state = State::new();
        let exporter = exporter_with(&state);
        let mut record = record(1);
        record.set_body(AnyValue::ListAny(Box::new(vec![
            AnyValue::Int(0);
            opentelemetry_c_abi::OTEL_LOG_MAX_ARRAY_ELEMENTS
                + 1
        ])));
        let scope = opentelemetry::InstrumentationScope::builder("test").build();
        let entries = [(&record, &scope)];
        assert!(matches!(
            exporter.export(LogBatch::new(&entries)),
            Err(OTelSdkError::InternalFailure(_))
        ));
        assert_eq!(state.exports.load(Ordering::SeqCst), 0);
        drop(exporter);
    }

    struct ConcurrentState {
        entered: Barrier,
        release: Barrier,
        exports: AtomicUsize,
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
    }

    extern "C" fn blocking_export(
        user_data: *mut c_void,
        _batch: *const OtelLogExportBatchView,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ConcurrentState>()) };
        state.exports.fetch_add(1, Ordering::SeqCst);
        state.entered.wait();
        state.release.wait();
        OtelStatus::Ok
    }

    extern "C" fn concurrent_shutdown(user_data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ConcurrentState>()) };
        state.shutdowns.fetch_add(1, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn concurrent_destroy(user_data: *mut c_void) {
        let state = unsafe { Arc::from_raw(user_data.cast::<ConcurrentState>()) };
        state.destroys.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn shutdown_waits_for_an_in_flight_export() {
        let state = Arc::new(ConcurrentState {
            entered: Barrier::new(2),
            release: Barrier::new(2),
            exports: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
        });
        let exporter = Arc::new(CustomLogExporter {
            callbacks: OtelCustomLogExporterCallbacks {
                struct_size: std::mem::size_of::<OtelCustomLogExporterCallbacks>(),
                export_logs: Some(blocking_export),
                shutdown: Some(concurrent_shutdown),
                state_destroy: Some(concurrent_destroy),
            },
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        });

        let exporting = {
            let exporter = Arc::clone(&exporter);
            std::thread::spawn(move || exporter.export(LogBatch::new(&[])))
        };
        state.entered.wait();
        let shutting_down = {
            let exporter = Arc::clone(&exporter);
            std::thread::spawn(move || exporter.shutdown(Duration::from_secs(1)))
        };
        std::thread::sleep(Duration::from_millis(20));
        // The shutdown callback cannot run while the export callback holds the read lock.
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 0);
        state.release.wait();
        assert!(exporting.join().unwrap().is_ok());
        assert!(shutting_down.join().unwrap().is_ok());
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        drop(exporter);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }
}
