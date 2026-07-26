//! Application-controlled Metrics reader.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use opentelemetry_c_abi::{OtelHandleHeader, OtelStatus, OTEL_HANDLE_KIND_MANUAL_METRIC_READER};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{ManualReader, Pipeline, Temporality};

use crate::error::{clear_last_error, fail};
use crate::handle::{guard_status, guard_unit, into_raw, take, HasHandleHeader};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

pub(crate) struct ManualMetricReader {
    reader: ManualReader,
    exporter: MetricExporterImpl,
    registered: AtomicBool,
    shutdown: AtomicBool,
}

impl fmt::Debug for ManualMetricReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManualMetricReader")
    }
}

impl ManualMetricReader {
    fn collect_and_export(&self) -> OTelSdkResult {
        let mut metrics = ResourceMetrics::default();
        self.reader.collect(&mut metrics)?;
        futures_executor::block_on(self.exporter.export(&metrics))
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.collect_and_export()?;
        self.exporter.force_flush()
    }

    fn shutdown(&self, timeout: Duration) -> OTelSdkResult {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return Err(opentelemetry_sdk::error::OTelSdkError::AlreadyShutdown);
        }
        let reader_result = self.reader.shutdown_with_timeout(timeout);
        let exporter_result = self.exporter.shutdown_with_timeout(timeout);
        reader_result.and(exporter_result)
    }
}

#[derive(Clone)]
pub(crate) struct SharedManualMetricReader(Arc<ManualMetricReader>);

impl SharedManualMetricReader {
    pub(crate) fn new(reader: Arc<ManualMetricReader>) -> Self {
        Self(reader)
    }

    pub(crate) fn shutdown_unregistered(self) {
        if !self.0.registered.load(Ordering::Acquire) {
            let _ = self.0.shutdown(Duration::from_secs(5));
        }
    }
}

impl fmt::Debug for SharedManualMetricReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SharedManualMetricReader")
    }
}

impl MetricReader for SharedManualMetricReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.registered.store(true, Ordering::Release);
        self.0.reader.register_pipeline(pipeline);
    }

    fn collect(&self, metrics: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.reader.collect(metrics)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown(timeout)
    }

    fn temporality(&self, _kind: opentelemetry_sdk::metrics::InstrumentKind) -> Temporality {
        self.0.exporter.temporality()
    }
}

#[repr(C)]
pub struct OtelManualMetricReader {
    header: OtelHandleHeader,
    pub(crate) reader: SharedManualMetricReader,
}

impl HasHandleHeader for OtelManualMetricReader {
    const KIND: u64 = OTEL_HANDLE_KIND_MANUAL_METRIC_READER;

    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }

    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Transfer an exporter into a manual reader.
///
/// # Safety
///
/// `exporter` must be a live exporter handle and `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_manual_metric_reader_new(
    exporter: *mut OtelMetricExporter,
    out: *mut *mut OtelManualMetricReader,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let exporter = match unsafe { take(exporter) } {
            Some(exporter) => exporter.exporter,
            None => return OtelStatus::InvalidArgument,
        };
        let temporality = exporter.temporality();
        let reader = ManualReader::builder()
            .with_temporality(temporality)
            .build();
        unsafe {
            *out = into_raw(OtelManualMetricReader {
                header: OtelHandleHeader::new(OtelManualMetricReader::KIND),
                reader: SharedManualMetricReader::new(Arc::new(ManualMetricReader {
                    reader,
                    exporter,
                    registered: AtomicBool::new(false),
                    shutdown: AtomicBool::new(false),
                })),
            });
        }
        OtelStatus::Ok
    })
}

/// Destroy an untransferred manual reader, shutting down its exporter.
///
/// # Safety
///
/// `reader` must be NULL or a live reader handle and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_manual_metric_reader_destroy(reader: *mut OtelManualMetricReader) {
    guard_unit(|| {
        if let Some(reader) = unsafe { take(reader) } {
            reader.reader.shutdown_unregistered();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};
    use std::sync::Mutex;

    use opentelemetry_c_abi::OtelStringView;

    use crate::custom_metric_exporter::{
        otel_custom_metric_exporter_new, OtelCustomMetricExporterCallbacks,
    };
    use crate::metric_batch::{
        otel_metric_batch_visit, OtelMetricAttribute, OtelMetricBatch, OtelMetricMetadata,
        OtelMetricPoint, OtelMetricVisitor, OTEL_METRIC_DATA_SUM, OTEL_METRIC_NUMBER_U64,
    };
    use crate::sdk::{
        otel_sdk_build, otel_sdk_builder_add_manual_metric_reader, otel_sdk_builder_destroy,
        otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_get_meter_provider,
        otel_sdk_metrics_force_flush, otel_sdk_metrics_shutdown,
    };
    use opentelemetry_c_api as api;

    struct ExportState {
        exports: AtomicUsize,
        flushes: AtomicUsize,
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
        export_status: AtomicU32,
        stale_batch: AtomicUsize,
        cross_thread_status: AtomicU32,
        metric_name: Mutex<String>,
        value: AtomicU64,
    }

    fn view(value: &str) -> OtelStringView {
        OtelStringView {
            ptr: value.as_ptr().cast(),
            len: value.len(),
        }
    }

    extern "C" fn visit_metric(
        user_data: *mut c_void,
        metadata: *const OtelMetricMetadata,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ExportState>()) };
        let metadata = unsafe { &*metadata };
        assert_eq!(metadata.data_kind, OTEL_METRIC_DATA_SUM);
        assert_eq!(metadata.number_kind, OTEL_METRIC_NUMBER_U64);
        let name = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                metadata.name.ptr.cast(),
                metadata.name.len,
            ))
        };
        *state
            .metric_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = name.to_owned();
        OtelStatus::Ok
    }

    extern "C" fn visit_point(
        user_data: *mut c_void,
        point: *const OtelMetricPoint,
        _attributes: *const OtelMetricAttribute,
        _attribute_count: usize,
        _explicit_bounds: *const f64,
        _explicit_bound_count: usize,
        _explicit_bucket_counts: *const u64,
        _explicit_bucket_count: usize,
        _positive_bucket_counts: *const u64,
        _positive_bucket_count: usize,
        _negative_bucket_counts: *const u64,
        _negative_bucket_count: usize,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ExportState>()) };
        state
            .value
            .store(unsafe { (*point).value.u64_value }, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn export_metrics(
        user_data: *mut c_void,
        batch: *const OtelMetricBatch,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ExportState>()) };
        state.exports.fetch_add(1, Ordering::SeqCst);
        state.stale_batch.store(batch as usize, Ordering::SeqCst);
        let configured = state.export_status.load(Ordering::SeqCst);
        if configured != OtelStatus::Ok.0 {
            return OtelStatus(configured);
        }
        let visitor = OtelMetricVisitor {
            struct_size: std::mem::size_of::<OtelMetricVisitor>(),
            resource: None,
            scope: None,
            metric: Some(visit_metric),
            point: Some(visit_point),
            exemplar: None,
        };
        let batch_token = batch as usize;
        let cross_thread_status = std::thread::spawn(move || {
            let visitor = OtelMetricVisitor {
                struct_size: std::mem::size_of::<OtelMetricVisitor>(),
                resource: None,
                scope: None,
                metric: None,
                point: None,
                exemplar: None,
            };
            unsafe {
                otel_metric_batch_visit(
                    batch_token as *const OtelMetricBatch,
                    &visitor,
                    std::ptr::null_mut(),
                )
            }
        })
        .join()
        .unwrap();
        state
            .cross_thread_status
            .store(cross_thread_status.0, Ordering::SeqCst);
        unsafe { otel_metric_batch_visit(batch, &visitor, user_data) }
    }

    extern "C" fn force_flush(user_data: *mut c_void) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ExportState>()) };
        state.flushes.fetch_add(1, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn shutdown(user_data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<ExportState>()) };
        state.shutdowns.fetch_add(1, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn destroy_state(user_data: *mut c_void) {
        let state = unsafe { Arc::from_raw(user_data.cast::<ExportState>()) };
        state.destroys.fetch_add(1, Ordering::SeqCst);
    }

    fn custom_exporter(state: &Arc<ExportState>) -> *mut OtelMetricExporter {
        let callbacks = OtelCustomMetricExporterCallbacks {
            struct_size: std::mem::size_of::<OtelCustomMetricExporterCallbacks>(),
            export_metrics: Some(export_metrics),
            force_flush: Some(force_flush),
            shutdown: Some(shutdown),
            state_destroy: Some(destroy_state),
        };
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_metric_exporter_new(
                    &callbacks,
                    Arc::into_raw(Arc::clone(state)).cast_mut().cast(),
                    1,
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        exporter
    }

    fn build_manual_sdk(state: &Arc<ExportState>) -> *mut crate::sdk::OtelSdk {
        let exporter = custom_exporter(state);
        let mut reader = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_manual_metric_reader_new(exporter, &mut reader) },
            OtelStatus::Ok
        );
        let builder = otel_sdk_builder_new();
        assert_eq!(
            unsafe { otel_sdk_builder_add_manual_metric_reader(builder, reader) },
            OtelStatus::Ok
        );
        let mut sdk = std::ptr::null_mut();
        assert_eq!(unsafe { otel_sdk_build(builder, &mut sdk) }, OtelStatus::Ok);
        unsafe { otel_sdk_builder_destroy(builder) };
        sdk
    }

    fn state() -> Arc<ExportState> {
        Arc::new(ExportState {
            exports: AtomicUsize::new(0),
            flushes: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            export_status: AtomicU32::new(OtelStatus::Ok.0),
            stale_batch: AtomicUsize::new(0),
            cross_thread_status: AtomicU32::new(OtelStatus::Ok.0),
            metric_name: Mutex::new(String::new()),
            value: AtomicU64::new(0),
        })
    }

    #[test]
    fn manual_reader_collects_exports_and_releases_callback_state_once() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = state();
        let sdk = build_manual_sdk(&state);
        let provider_ctx = unsafe { otel_sdk_get_meter_provider(sdk) };
        let provider = unsafe {
            api::otel_api_meter_provider_new(crate::metrics_vtable::vtable_ptr(), provider_ctx)
        };
        let meter = unsafe {
            api::otel_meter_provider_get_meter(
                provider,
                view("custom-exporter-test"),
                OtelStringView::empty(),
                OtelStringView::empty(),
            )
        };
        assert!(!meter.is_null());
        let options = api::OtelInstrumentOptions {
            struct_size: std::mem::size_of::<api::OtelInstrumentOptions>() as u64,
            description: OtelStringView::empty(),
            unit: OtelStringView::empty(),
            boundaries: std::ptr::null(),
            boundary_count: 0,
        };
        let mut counter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                api::otel_meter_create_u64_counter(meter, view("requests"), &options, &mut counter)
            },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe { api::otel_counter_u64_add(counter, 7, std::ptr::null(), 0) },
            OtelStatus::Ok
        );

        assert_eq!(
            unsafe { otel_sdk_metrics_force_flush(sdk, 0) },
            OtelStatus::Ok
        );
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.flushes.load(Ordering::SeqCst), 1);
        assert_eq!(
            *state
                .metric_name
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            "requests"
        );
        assert_eq!(state.value.load(Ordering::SeqCst), 7);
        assert_eq!(
            state.cross_thread_status.load(Ordering::SeqCst),
            OtelStatus::InvalidArgument.0
        );

        let visitor = OtelMetricVisitor {
            struct_size: std::mem::size_of::<OtelMetricVisitor>(),
            resource: None,
            scope: None,
            metric: None,
            point: None,
            exemplar: None,
        };
        assert_eq!(
            unsafe {
                otel_metric_batch_visit(
                    state.stale_batch.load(Ordering::SeqCst) as *const OtelMetricBatch,
                    &visitor,
                    std::ptr::null_mut(),
                )
            },
            OtelStatus::InvalidArgument
        );

        unsafe {
            api::otel_counter_u64_destroy(counter);
            api::otel_meter_destroy(meter);
            api::otel_meter_provider_destroy(provider);
        }
        assert_eq!(
            unsafe { otel_sdk_metrics_shutdown(sdk, 1000) },
            OtelStatus::Ok
        );
        assert_eq!(state.exports.load(Ordering::SeqCst), 1);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        unsafe { otel_sdk_destroy(sdk) };
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn untransferred_manual_reader_shuts_down_and_releases_exporter() {
        let state = state();
        let exporter = custom_exporter(&state);
        let mut reader = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_manual_metric_reader_new(exporter, &mut reader) },
            OtelStatus::Ok
        );
        unsafe { otel_manual_metric_reader_destroy(reader) };
        assert_eq!(state.exports.load(Ordering::SeqCst), 0);
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn multiple_manual_readers_collect_independently() {
        let first = state();
        let second = state();
        let builder = otel_sdk_builder_new();
        for state in [&first, &second] {
            let exporter = custom_exporter(state);
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                unsafe { otel_manual_metric_reader_new(exporter, &mut reader) },
                OtelStatus::Ok
            );
            assert_eq!(
                unsafe { otel_sdk_builder_add_manual_metric_reader(builder, reader) },
                OtelStatus::Ok
            );
        }
        let mut sdk = std::ptr::null_mut();
        assert_eq!(unsafe { otel_sdk_build(builder, &mut sdk) }, OtelStatus::Ok);
        unsafe { otel_sdk_builder_destroy(builder) };

        assert_eq!(
            unsafe { otel_sdk_metrics_force_flush(sdk, 0) },
            OtelStatus::Ok
        );
        assert_eq!(first.exports.load(Ordering::SeqCst), 1);
        assert_eq!(second.exports.load(Ordering::SeqCst), 1);
        unsafe {
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 1000), OtelStatus::Ok);
            otel_sdk_destroy(sdk);
        }
        assert_eq!(first.destroys.load(Ordering::SeqCst), 1);
        assert_eq!(second.destroys.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn custom_export_statuses_propagate_through_manual_collection() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = state();
        let sdk = build_manual_sdk(&state);

        state
            .export_status
            .store(OtelStatus::Timeout.0, Ordering::SeqCst);
        assert_eq!(
            unsafe { otel_sdk_metrics_force_flush(sdk, 1) },
            OtelStatus::ExportFailed
        );
        state
            .export_status
            .store(OtelStatus::ExportFailed.0, Ordering::SeqCst);
        assert_eq!(
            unsafe { otel_sdk_metrics_force_flush(sdk, 1) },
            OtelStatus::ExportFailed
        );

        state
            .export_status
            .store(OtelStatus::Ok.0, Ordering::SeqCst);
        unsafe {
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 1000), OtelStatus::Ok);
            otel_sdk_destroy(sdk);
        }
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);
    }
}
