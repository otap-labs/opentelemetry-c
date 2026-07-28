//! C callback-backed Metrics exporter.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::RwLock;
use std::time::Duration;

use opentelemetry_c_abi::OtelStatus;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::Temporality;

use crate::error::{clear_last_error, fail};
use crate::handle::{guard_status, into_raw};
use crate::metric_batch::{MetricBatchRegistration, OtelMetricBatch};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

pub type OtelCustomMetricExport =
    Option<extern "C" fn(*mut c_void, *const OtelMetricBatch) -> OtelStatus>;
pub type OtelCustomMetricForceFlush = Option<extern "C" fn(*mut c_void) -> OtelStatus>;
pub type OtelCustomMetricShutdown = Option<extern "C" fn(*mut c_void, u64) -> OtelStatus>;
pub type OtelCustomMetricStateDestroy = Option<extern "C" fn(*mut c_void)>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelCustomMetricExporterCallbacks {
    pub struct_size: usize,
    pub export_metrics: OtelCustomMetricExport,
    pub force_flush: OtelCustomMetricForceFlush,
    pub shutdown: OtelCustomMetricShutdown,
    pub state_destroy: OtelCustomMetricStateDestroy,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelCustomMetricExporterCallbacks>() == 40);
    assert!(std::mem::align_of::<OtelCustomMetricExporterCallbacks>() == 8);
    assert!(std::mem::offset_of!(OtelCustomMetricExporterCallbacks, struct_size) == 0);
    assert!(std::mem::offset_of!(OtelCustomMetricExporterCallbacks, export_metrics) == 8);
    assert!(std::mem::offset_of!(OtelCustomMetricExporterCallbacks, force_flush) == 16);
    assert!(std::mem::offset_of!(OtelCustomMetricExporterCallbacks, shutdown) == 24);
    assert!(std::mem::offset_of!(OtelCustomMetricExporterCallbacks, state_destroy) == 32);
    assert!(REQUIRED_PREFIX_SIZE == 16);
};

/// End offset of a member, i.e. the smallest `struct_size` that fully contains it.
macro_rules! member_end {
    ($field:ident, $ty:ty) => {
        std::mem::offset_of!(OtelCustomMetricExporterCallbacks, $field) + std::mem::size_of::<$ty>()
    };
}

/// Smallest accepted callback table: `struct_size` plus mandatory `export_metrics`.
const REQUIRED_PREFIX_SIZE: usize = member_end!(export_metrics, OtelCustomMetricExport);
const FORCE_FLUSH_END: usize = member_end!(force_flush, OtelCustomMetricForceFlush);
const SHUTDOWN_END: usize = member_end!(shutdown, OtelCustomMetricShutdown);
const STATE_DESTROY_END: usize = member_end!(state_destroy, OtelCustomMetricStateDestroy);

/// Copy only callback members covered by the caller's `struct_size`.
///
/// # Safety
///
/// `callbacks` must address at least `struct_size` readable bytes and `struct_size` must be at
/// least [`REQUIRED_PREFIX_SIZE`].
unsafe fn read_callbacks(
    callbacks: *const OtelCustomMetricExporterCallbacks,
    struct_size: usize,
) -> OtelCustomMetricExporterCallbacks {
    let base = callbacks.cast::<u8>();
    // SAFETY: every member is read only after `struct_size` proves it is present. Unaligned
    // reads also support packed or otherwise misaligned caller storage.
    unsafe {
        let member = |offset: usize, end: usize| {
            (struct_size >= end).then(|| base.add(offset).cast::<usize>())
        };
        OtelCustomMetricExporterCallbacks {
            struct_size,
            export_metrics: std::ptr::read_unaligned(
                base.add(std::mem::offset_of!(
                    OtelCustomMetricExporterCallbacks,
                    export_metrics
                ))
                .cast::<OtelCustomMetricExport>(),
            ),
            force_flush: member(
                std::mem::offset_of!(OtelCustomMetricExporterCallbacks, force_flush),
                FORCE_FLUSH_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomMetricForceFlush>())
            }),
            shutdown: member(
                std::mem::offset_of!(OtelCustomMetricExporterCallbacks, shutdown),
                SHUTDOWN_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomMetricShutdown>())
            }),
            state_destroy: member(
                std::mem::offset_of!(OtelCustomMetricExporterCallbacks, state_destroy),
                STATE_DESTROY_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomMetricStateDestroy>())
            }),
        }
    }
}

pub(crate) struct CustomMetricExporter {
    callbacks: OtelCustomMetricExporterCallbacks,
    user_data: *mut c_void,
    temporality: Temporality,
    shutdown: RwLock<bool>,
}

unsafe impl Send for CustomMetricExporter {}
unsafe impl Sync for CustomMetricExporter {}

impl std::fmt::Debug for CustomMetricExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CustomMetricExporter")
    }
}

impl CustomMetricExporter {
    fn callback_result(
        &self,
        operation: &str,
        timeout: Duration,
        callback: impl FnOnce() -> OtelStatus,
    ) -> OTelSdkResult {
        let status = catch_unwind(AssertUnwindSafe(callback)).map_err(|_| {
            OTelSdkError::InternalFailure(format!(
                "custom metric exporter {operation} callback panicked"
            ))
        })?;
        match status {
            OtelStatus::Ok => Ok(()),
            OtelStatus::AlreadyShutdown => Err(OTelSdkError::AlreadyShutdown),
            OtelStatus::Timeout => Err(OTelSdkError::Timeout(timeout)),
            status => Err(OTelSdkError::InternalFailure(format!(
                "custom metric exporter {operation} callback failed with status {}",
                status.0
            ))),
        }
    }

    fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let shutdown = self
            .shutdown
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        let registration = MetricBatchRegistration::new(metrics);
        let Some(callback) = self.callbacks.export_metrics else {
            return Err(OTelSdkError::InternalFailure(
                "custom metric exporter lost its validated export callback".to_owned(),
            ));
        };
        self.callback_result("export", Duration::ZERO, || {
            callback(self.user_data, registration.token())
        })
    }

    fn force_flush(&self) -> OTelSdkResult {
        let shutdown = self
            .shutdown
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        match self.callbacks.force_flush {
            Some(callback) => {
                self.callback_result("force-flush", Duration::ZERO, || callback(self.user_data))
            }
            None => Ok(()),
        }
    }

    fn shutdown(&self, timeout: Duration) -> OTelSdkResult {
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

    pub(crate) fn temporality(&self) -> Temporality {
        self.temporality
    }
}

impl Drop for CustomMetricExporter {
    fn drop(&mut self) {
        let _ = self.shutdown(Duration::from_secs(5));
        if let Some(destroy) = self.callbacks.state_destroy {
            let _ = catch_unwind(AssertUnwindSafe(|| destroy(self.user_data)));
        }
    }
}

pub(crate) fn custom_exporter_export(
    exporter: &CustomMetricExporter,
    metrics: &ResourceMetrics,
) -> OTelSdkResult {
    exporter.export(metrics)
}

pub(crate) fn custom_exporter_force_flush(exporter: &CustomMetricExporter) -> OTelSdkResult {
    exporter.force_flush()
}

pub(crate) fn custom_exporter_shutdown(
    exporter: &CustomMetricExporter,
    timeout: Duration,
) -> OTelSdkResult {
    exporter.shutdown(timeout)
}

fn configured_temporality(value: u32) -> Option<Temporality> {
    match value {
        0 | 1 => Some(Temporality::Cumulative),
        2 => Some(Temporality::Delta),
        3 => Some(Temporality::LowMemory),
        _ => None,
    }
}

/// Create an exporter backed by thread-safe C callbacks.
///
/// # Safety
///
/// `callbacks` must address a readable callback structure. `out` must address writable
/// storage. Callback state must remain valid until `state_destroy` is invoked.
#[no_mangle]
pub unsafe extern "C" fn otel_custom_metric_exporter_new(
    callbacks: *const OtelCustomMetricExporterCallbacks,
    user_data: *mut c_void,
    temporality: u32,
    out: *mut *mut OtelMetricExporter,
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
                "custom metric exporter callbacks must not be NULL",
            );
        }
        let struct_size = unsafe { std::ptr::read_unaligned(callbacks.cast::<usize>()) };
        if struct_size < REQUIRED_PREFIX_SIZE {
            return fail(
                OtelStatus::InvalidConfig,
                "custom metric exporter callback structure is smaller than the required ABI size",
            );
        }
        let callbacks = unsafe { read_callbacks(callbacks, struct_size) };
        if callbacks.export_metrics.is_none() {
            return fail(
                OtelStatus::InvalidConfig,
                "custom metric exporter requires an export callback",
            );
        }
        let temporality = match configured_temporality(temporality) {
            Some(temporality) => temporality,
            None => {
                return fail(
                    OtelStatus::InvalidConfig,
                    "unknown custom metric exporter temporality",
                )
            }
        };
        let exporter = CustomMetricExporter {
            callbacks,
            user_data,
            temporality,
            shutdown: RwLock::new(false),
        };
        unsafe {
            *out = into_raw(OtelMetricExporter::new(MetricExporterImpl::Custom(
                exporter,
            )))
        };
        OtelStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use crate::metric_exporter::otel_metric_exporter_destroy;
    use crate::periodic_metric_reader::{
        otel_periodic_metric_reader_builder_build, otel_periodic_metric_reader_builder_destroy,
        otel_periodic_metric_reader_builder_new, otel_periodic_metric_reader_builder_set_exporter,
        otel_periodic_metric_reader_builder_set_interval_millis,
        otel_periodic_metric_reader_destroy,
    };

    struct LifecycleState {
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
        shutdown_status: OtelStatus,
    }

    extern "C" fn export_ok(_user_data: *mut c_void, _batch: *const OtelMetricBatch) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn force_flush_ok(_user_data: *mut c_void) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn shutdown_state(user_data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<LifecycleState>()) };
        state.shutdowns.fetch_add(1, Ordering::SeqCst);
        state.shutdown_status
    }

    extern "C" fn destroy_state(user_data: *mut c_void) {
        let state = unsafe { Arc::from_raw(user_data.cast::<LifecycleState>()) };
        state.destroys.fetch_add(1, Ordering::SeqCst);
    }

    fn callbacks() -> OtelCustomMetricExporterCallbacks {
        OtelCustomMetricExporterCallbacks {
            struct_size: std::mem::size_of::<OtelCustomMetricExporterCallbacks>(),
            export_metrics: Some(export_ok),
            force_flush: None,
            shutdown: Some(shutdown_state),
            state_destroy: Some(destroy_state),
        }
    }

    /// Copy the callback table into an allocation of exactly `prefix_size` bytes. Any read
    /// beyond the declared table is therefore a real out-of-bounds access under ASan or Miri.
    fn truncated_table(prefix_size: usize) -> Box<[usize]> {
        let word = std::mem::size_of::<usize>();
        assert_eq!(prefix_size % word, 0);
        let mut full = callbacks();
        full.force_flush = Some(force_flush_ok);
        let words = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&full).cast::<usize>(),
                std::mem::size_of::<OtelCustomMetricExporterCallbacks>() / word,
            )
        };
        let mut table = words[..prefix_size / word].to_vec();
        table[0] = prefix_size;
        table.into_boxed_slice()
    }

    fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while counter.load(Ordering::SeqCst) != expected {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for callback count {expected}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn untransferred_exporter_shuts_down_and_destroys_state_once() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Ok,
        });
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(
                    &callbacks(),
                    Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
                    0,
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        unsafe { otel_metric_exporter_destroy(exporter) };
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn custom_exporter_builds_a_periodic_reader_without_otlp() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Ok,
        });
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(
                    &callbacks(),
                    Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
                    0,
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        let builder = otel_periodic_metric_reader_builder_new();
        assert_eq!(
            unsafe { otel_periodic_metric_reader_builder_set_interval_millis(builder, 60_000) },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe { otel_periodic_metric_reader_builder_set_exporter(builder, exporter) },
            OtelStatus::Ok
        );
        let mut reader = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_periodic_metric_reader_builder_build(builder, &mut reader) },
            OtelStatus::Ok
        );
        unsafe {
            otel_periodic_metric_reader_builder_destroy(builder);
            otel_periodic_metric_reader_destroy(reader);
        }
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        wait_for_count(&state.destroys, 1);
    }

    #[test]
    fn construction_failure_preserves_callback_state_ownership() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Ok,
        });
        let raw = Arc::into_raw(Arc::clone(&state));
        let mut invalid = callbacks();
        invalid.struct_size = 0;
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(&invalid, raw.cast_mut().cast(), 0, &mut exporter)
            },
            OtelStatus::InvalidConfig
        );
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::SeqCst), 0);
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn callback_table_required_prefix_omits_every_optional_member() {
        let table = truncated_table(REQUIRED_PREFIX_SIZE);
        let parsed = unsafe { read_callbacks(table.as_ptr().cast(), REQUIRED_PREFIX_SIZE) };
        assert!(parsed.export_metrics.is_some());
        assert!(parsed.force_flush.is_none());
        assert!(parsed.shutdown.is_none());
        assert!(parsed.state_destroy.is_none());
    }

    #[test]
    fn constructor_accepts_the_required_prefix_without_taking_optional_callbacks() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Ok,
        });
        let raw = Arc::into_raw(Arc::clone(&state));
        let table = truncated_table(REQUIRED_PREFIX_SIZE);
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(
                    table.as_ptr().cast(),
                    raw.cast_mut().cast(),
                    0,
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        unsafe { otel_metric_exporter_destroy(exporter) };
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 0);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 0);
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn callback_table_force_flush_prefix_reads_only_force_flush() {
        let table = truncated_table(FORCE_FLUSH_END);
        let parsed = unsafe { read_callbacks(table.as_ptr().cast(), FORCE_FLUSH_END) };
        assert!(parsed.export_metrics.is_some());
        assert!(parsed.force_flush.is_some());
        assert!(parsed.shutdown.is_none());
        assert!(parsed.state_destroy.is_none());
    }

    #[test]
    fn callback_table_shutdown_prefix_reads_through_shutdown() {
        let table = truncated_table(SHUTDOWN_END);
        let parsed = unsafe { read_callbacks(table.as_ptr().cast(), SHUTDOWN_END) };
        assert!(parsed.export_metrics.is_some());
        assert!(parsed.force_flush.is_some());
        assert!(parsed.shutdown.is_some());
        assert!(parsed.state_destroy.is_none());
    }

    #[test]
    fn callback_table_full_and_longer_sizes_are_accepted() {
        let full = truncated_table(STATE_DESTROY_END);
        let parsed = unsafe { read_callbacks(full.as_ptr().cast(), STATE_DESTROY_END) };
        assert!(parsed.state_destroy.is_some());

        let mut longer = full.into_vec();
        longer.push(0xdead_beef);
        longer.push(0xfeed_face);
        let longer_size = longer.len() * std::mem::size_of::<usize>();
        longer[0] = longer_size;
        let parsed = unsafe { read_callbacks(longer.as_ptr().cast(), longer_size) };
        assert!(parsed.export_metrics.is_some());
        assert!(parsed.force_flush.is_some());
        assert!(parsed.shutdown.is_some());
        assert!(parsed.state_destroy.is_some());
    }

    #[test]
    fn constructor_rejects_a_table_shorter_than_the_required_prefix() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Ok,
        });
        let raw = Arc::into_raw(Arc::clone(&state));
        let mut table = truncated_table(REQUIRED_PREFIX_SIZE);
        table[0] = REQUIRED_PREFIX_SIZE - 1;
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(
                    table.as_ptr().cast(),
                    raw.cast_mut().cast(),
                    0,
                    &mut exporter,
                )
            },
            OtelStatus::InvalidConfig
        );
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::SeqCst), 0);
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn callback_timeout_is_preserved_at_the_exporter_boundary() {
        let state = Arc::new(LifecycleState {
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            shutdown_status: OtelStatus::Timeout,
        });
        let exporter = CustomMetricExporter {
            callbacks: callbacks(),
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            temporality: Temporality::Cumulative,
            shutdown: RwLock::new(false),
        };
        assert!(matches!(
            custom_exporter_shutdown(&exporter, Duration::from_millis(17)),
            Err(OTelSdkError::Timeout(timeout)) if timeout == Duration::from_millis(17)
        ));
        drop(exporter);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    struct ConcurrentState {
        export_entered: Barrier,
        release_export: Barrier,
        exports: AtomicUsize,
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
    }

    extern "C" fn blocking_export(
        user_data: *mut c_void,
        _batch: *const OtelMetricBatch,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ConcurrentState>()) };
        state.exports.fetch_add(1, Ordering::SeqCst);
        state.export_entered.wait();
        state.release_export.wait();
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
    fn shutdown_waits_for_in_flight_export_and_blocks_later_callbacks() {
        let state = Arc::new(ConcurrentState {
            export_entered: Barrier::new(2),
            release_export: Barrier::new(2),
            exports: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
        });
        let exporter = Arc::new(CustomMetricExporter {
            callbacks: OtelCustomMetricExporterCallbacks {
                struct_size: std::mem::size_of::<OtelCustomMetricExporterCallbacks>(),
                export_metrics: Some(blocking_export),
                force_flush: None,
                shutdown: Some(concurrent_shutdown),
                state_destroy: Some(concurrent_destroy),
            },
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            temporality: Temporality::Cumulative,
            shutdown: RwLock::new(false),
        });

        let exporting = {
            let exporter = Arc::clone(&exporter);
            std::thread::spawn(move || {
                custom_exporter_export(&exporter, &ResourceMetrics::default())
            })
        };
        state.export_entered.wait();
        let shutting_down = {
            let exporter = Arc::clone(&exporter);
            std::thread::spawn(move || custom_exporter_shutdown(&exporter, Duration::from_secs(1)))
        };
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 0);
        state.release_export.wait();
        assert!(exporting.join().unwrap().is_ok());
        assert!(shutting_down.join().unwrap().is_ok());
        assert!(matches!(
            custom_exporter_export(&exporter, &ResourceMetrics::default()),
            Err(OTelSdkError::AlreadyShutdown)
        ));
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        drop(exporter);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }
}
