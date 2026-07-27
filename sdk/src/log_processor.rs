//! The generic log-processor handle (`otel_log_processor_t`) and the simple/batch builders.
//!
//! Like [`crate::span_processor`], the opaque C handle wraps an internal enum of concrete
//! processor kinds, so the SDK builder stores a homogeneous `Vec<LogProcessorImpl>` and a new
//! processor kind never changes the public C ABI.

use std::time::Duration;

use opentelemetry::logs::Severity;
use opentelemetry::InstrumentationScope;
use opentelemetry_c_abi::{
    OtelHandleHeader, OtelStatus, OTEL_HANDLE_KIND_BATCH_LOG_PROCESSOR_BUILDER,
    OTEL_HANDLE_KIND_LOG_PROCESSOR,
};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{
    BatchConfigBuilder, BatchLogProcessor, LogProcessor, LoggerProviderBuilder, SdkLogRecord,
    SimpleLogProcessor,
};
use opentelemetry_sdk::Resource;

use crate::error::{clear_last_error, fail};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, take, HasHandleHeader,
};
use crate::log_exporter::{LogExporterImpl, OtelLogExporter};

/// Internal log-processor implementation. Each variant is a concrete processor kind.
#[derive(Debug)]
pub(crate) enum LogProcessorImpl {
    /// Exports each record on the emitting thread, before `otel_logger_emit` returns.
    /// Boxed: the pinned simple processor embeds the exporter inline and is far larger than
    /// the batch handle, which would otherwise bloat every `LogProcessorImpl` slot.
    Simple(Box<SimpleLogProcessor<LogExporterImpl>>),
    /// Queues records and exports them from a dedicated OS thread.
    Batch(BatchLogProcessor),
}

impl LogProcessorImpl {
    /// Install this processor into a logger-provider builder.
    ///
    /// The concrete type is preserved instead of boxing into `dyn LogProcessor`, so the
    /// pinned SDK keeps its own static dispatch.
    pub(crate) fn install(self, builder: LoggerProviderBuilder) -> LoggerProviderBuilder {
        match self {
            LogProcessorImpl::Simple(processor) => builder.with_log_processor(*processor),
            LogProcessorImpl::Batch(processor) => builder.with_log_processor(processor),
        }
    }
}

impl LogProcessor for LogProcessorImpl {
    fn emit(&self, data: &mut SdkLogRecord, instrumentation: &InstrumentationScope) {
        match self {
            LogProcessorImpl::Simple(p) => p.emit(data, instrumentation),
            LogProcessorImpl::Batch(p) => p.emit(data, instrumentation),
        }
    }
    fn force_flush(&self) -> OTelSdkResult {
        match self {
            LogProcessorImpl::Simple(p) => p.force_flush(),
            LogProcessorImpl::Batch(p) => p.force_flush(),
        }
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            LogProcessorImpl::Simple(p) => p.shutdown_with_timeout(timeout),
            LogProcessorImpl::Batch(p) => p.shutdown_with_timeout(timeout),
        }
    }
    fn event_enabled(&self, level: Severity, target: &str, name: Option<&str>) -> bool {
        match self {
            LogProcessorImpl::Simple(p) => p.event_enabled(level, target, name),
            LogProcessorImpl::Batch(p) => p.event_enabled(level, target, name),
        }
    }
    fn set_resource(&mut self, resource: &Resource) {
        match self {
            LogProcessorImpl::Simple(p) => p.set_resource(resource),
            LogProcessorImpl::Batch(p) => p.set_resource(resource),
        }
    }
}

/// Opaque log-processor handle. Owns a built processor until the SDK builder consumes it via
/// `otel_sdk_builder_add_log_processor`, or until it is destroyed.
#[repr(C)]
pub struct OtelLogProcessor {
    header: OtelHandleHeader,
    pub(crate) processor: LogProcessorImpl,
}

impl OtelLogProcessor {
    pub(crate) fn new(processor: LogProcessorImpl) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            processor,
        }
    }
}

impl HasHandleHeader for OtelLogProcessor {
    const KIND: u64 = OTEL_HANDLE_KIND_LOG_PROCESSOR;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Destroy a log-processor handle (no-op on NULL).
///
/// Dropping an untransferred batch processor disconnects its worker, but is not a draining
/// shutdown: queued records may be discarded and exporter shutdown is not guaranteed.
///
/// Do **not** call this on a processor already transferred into an SDK builder.
///
/// # Safety
///
/// `processor` must be NULL or a live log-processor handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_log_processor_destroy(processor: *mut OtelLogProcessor) {
    guard_unit(|| unsafe { destroy(processor) });
}

/// Build a **simple** log processor that takes ownership of `exporter`.
///
/// A simple processor exports synchronously on the thread that called `otel_logger_emit`, so
/// it serializes emitting threads behind one exporter. It is intended for tests, short-lived
/// programs, and debugging; production pipelines should prefer the batch processor.
///
/// On `OTEL_STATUS_OK` ownership of `exporter` transfers and the original pointer becomes
/// invalid. On failure the caller still owns `exporter`.
///
/// # Safety
///
/// `out` must address writable storage; `exporter` must be NULL or a live log-exporter handle
/// not used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_simple_log_processor_create(
    exporter: *mut OtelLogExporter,
    out: *mut *mut OtelLogProcessor,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        // Take ownership only after every other argument has been validated, so a rejected
        // call always leaves the exporter caller-owned.
        let owned = match unsafe { take::<OtelLogExporter>(exporter) } {
            Some(owned) => owned,
            None => return OtelStatus::InvalidArgument,
        };
        let processor = LogProcessorImpl::Simple(Box::new(SimpleLogProcessor::new(owned.exporter)));
        unsafe { *out = into_raw(OtelLogProcessor::new(processor)) };
        OtelStatus::Ok
    })
}

#[derive(Default)]
struct BatchLogProcessorConfig {
    exporter: Option<LogExporterImpl>,
    max_queue_size: Option<usize>,
    max_export_batch_size: Option<usize>,
    scheduled_delay_millis: Option<u64>,
}

/// Opaque batch log-processor builder. Not thread-safe; confine to one thread.
#[repr(C)]
pub struct OtelBatchLogProcessorBuilder {
    header: OtelHandleHeader,
    config: BatchLogProcessorConfig,
}

impl HasHandleHeader for OtelBatchLogProcessorBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_BATCH_LOG_PROCESSOR_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Create a batch log-processor builder. Release with
/// `otel_batch_log_processor_builder_destroy()`.
#[no_mangle]
pub extern "C" fn otel_batch_log_processor_builder_new() -> *mut OtelBatchLogProcessorBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelBatchLogProcessorBuilder {
            header: OtelHandleHeader::new(OtelBatchLogProcessorBuilder::KIND),
            config: BatchLogProcessorConfig::default(),
        })
    })
}

/// Destroy a batch log-processor builder (no-op on NULL). Any exporter still held by the
/// builder is released here.
///
/// # Safety
///
/// `builder` must be NULL or a live builder not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_destroy(
    builder: *mut OtelBatchLogProcessorBuilder,
) {
    guard_unit(|| unsafe { destroy(builder) });
}

/// Transfer an exporter into the builder, replacing (and releasing) any previous one.
///
/// On `OTEL_STATUS_OK` ownership of `exporter` transfers. On failure the caller still owns it.
///
/// # Safety
///
/// `builder` and `exporter` must satisfy the handle contract and must not be used
/// concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_set_exporter(
    builder: *mut OtelBatchLogProcessorBuilder,
    exporter: *mut OtelLogExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        // Validate the builder first so a bad builder leaves the exporter caller-owned.
        let builder = match unsafe { checked_mut::<OtelBatchLogProcessorBuilder>(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        let owned = match unsafe { take::<OtelLogExporter>(exporter) } {
            Some(owned) => owned,
            None => return OtelStatus::InvalidArgument,
        };
        builder.config.exporter = Some(owned.exporter);
        OtelStatus::Ok
    })
}

/// # Safety
/// `builder` must satisfy the handle contract (single-threaded).
unsafe fn with_config<F>(builder: *mut OtelBatchLogProcessorBuilder, f: F) -> OtelStatus
where
    F: FnOnce(&mut BatchLogProcessorConfig) -> OtelStatus,
{
    guard_status(|| {
        clear_last_error();
        match unsafe { checked_mut::<OtelBatchLogProcessorBuilder>(builder) } {
            Some(builder) => f(&mut builder.config),
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Set the maximum number of records buffered before new records are dropped
/// (`0` == SDK default).
///
/// # Safety
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_set_max_queue_size(
    builder: *mut OtelBatchLogProcessorBuilder,
    max_queue_size: usize,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.max_queue_size = (max_queue_size != 0).then_some(max_queue_size);
            OtelStatus::Ok
        })
    }
}

/// Set the maximum number of records exported per batch (`0` == SDK default).
///
/// # Safety
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_set_max_export_batch_size(
    builder: *mut OtelBatchLogProcessorBuilder,
    max_export_batch_size: usize,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.max_export_batch_size =
                (max_export_batch_size != 0).then_some(max_export_batch_size);
            OtelStatus::Ok
        })
    }
}

/// Set the delay between scheduled export cycles in milliseconds (`0` == SDK default).
///
/// # Safety
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_set_scheduled_delay_millis(
    builder: *mut OtelBatchLogProcessorBuilder,
    scheduled_delay_millis: u64,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.scheduled_delay_millis =
                (scheduled_delay_millis != 0).then_some(scheduled_delay_millis);
            OtelStatus::Ok
        })
    }
}

/// Build the batch processor, consuming the exporter held by the builder.
///
/// The builder remains caller-owned but loses its exporter, so a second build without setting
/// a new exporter fails with `OTEL_STATUS_INVALID_CONFIG`.
///
/// # Safety
///
/// `builder` must satisfy the handle contract; `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_batch_log_processor_builder_build(
    builder: *mut OtelBatchLogProcessorBuilder,
    out: *mut *mut OtelLogProcessor,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let builder = match unsafe { checked_mut::<OtelBatchLogProcessorBuilder>(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        // Cross-field validation before the exporter is consumed, so a rejected configuration
        // leaves the builder reusable rather than silently exporter-less.
        if let (Some(queue), Some(batch)) = (
            builder.config.max_queue_size,
            builder.config.max_export_batch_size,
        ) {
            if batch > queue {
                return fail(
                    OtelStatus::InvalidConfig,
                    "batch log processor max_export_batch_size must not exceed max_queue_size",
                );
            }
        }
        let Some(exporter) = builder.config.exporter.take() else {
            return fail(
                OtelStatus::InvalidConfig,
                "batch log processor requires an exporter",
            );
        };
        let mut batch_config = BatchConfigBuilder::default();
        if let Some(value) = builder.config.max_queue_size {
            batch_config = batch_config.with_max_queue_size(value);
        }
        if let Some(value) = builder.config.max_export_batch_size {
            batch_config = batch_config.with_max_export_batch_size(value);
        }
        if let Some(value) = builder.config.scheduled_delay_millis {
            batch_config = batch_config.with_scheduled_delay(Duration::from_millis(value));
        }
        // There is deliberately no per-export timeout setter: the pinned 0.32.1 synchronous
        // Logs `BatchConfigBuilder` exposes none (unlike the traces one), and accepting a
        // value that cannot be applied would be a false promise. This processor does not read
        // `OTEL_BLRP_EXPORT_TIMEOUT`; callers can bound the OTLP transport on its exporter.
        // The pinned builder spawns its worker OS thread here; the enclosing `guard_status`
        // converts a spawn panic into `OTEL_STATUS_INTERNAL_ERROR` rather than unwinding
        // across the C boundary.
        let processor = BatchLogProcessor::builder(exporter)
            .with_batch_config(batch_config.build())
            .build();
        unsafe { *out = into_raw(OtelLogProcessor::new(LogProcessorImpl::Batch(processor))) };
        OtelStatus::Ok
    })
}

/// Read-only probe used by tests to confirm a builder released its exporter.
#[cfg(test)]
pub(crate) fn builder_has_exporter(builder: *const OtelBatchLogProcessorBuilder) -> bool {
    unsafe { crate::handle::checked_ref::<OtelBatchLogProcessorBuilder>(builder) }
        .is_some_and(|builder| builder.config.exporter.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_exporter::otel_log_exporter_destroy;
    use opentelemetry_sdk::logs::InMemoryLogExporter;

    fn in_memory_exporter() -> (*mut OtelLogExporter, InMemoryLogExporter) {
        let exporter = InMemoryLogExporter::default();
        let handle = into_raw(OtelLogExporter::new(LogExporterImpl::InMemory(
            exporter.clone(),
        )));
        (handle, exporter)
    }

    #[test]
    fn simple_processor_rejects_null_out_and_leaves_exporter_owned() {
        unsafe {
            let (exporter, _probe) = in_memory_exporter();
            assert_eq!(
                otel_simple_log_processor_create(exporter, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            // Nothing was consumed, so the caller can still release it exactly once.
            otel_log_exporter_destroy(exporter);
        }
    }

    #[test]
    fn simple_processor_takes_ownership_only_on_success() {
        unsafe {
            let (exporter, _probe) = in_memory_exporter();
            let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();

            // Failure path: an already-destroyed exporter handle is rejected and no processor
            // is produced.
            let stale = into_raw(OtelLogExporter::new(LogExporterImpl::InMemory(
                InMemoryLogExporter::default(),
            )));
            otel_log_exporter_destroy(stale);
            assert_eq!(
                otel_simple_log_processor_create(stale, &mut processor),
                OtelStatus::InvalidArgument
            );
            assert!(processor.is_null());

            // Success path: ownership moves into the processor; destroying the processor is
            // the only release needed.
            assert_eq!(
                otel_simple_log_processor_create(exporter, &mut processor),
                OtelStatus::Ok
            );
            assert!(!processor.is_null());
            otel_log_processor_destroy(processor);
        }
    }

    #[test]
    fn batch_builder_rejects_bad_arguments_without_consuming_the_exporter() {
        unsafe {
            let builder = otel_batch_log_processor_builder_new();
            assert!(!builder.is_null());

            let (exporter, _probe) = in_memory_exporter();
            assert_eq!(
                otel_batch_log_processor_builder_set_exporter(std::ptr::null_mut(), exporter),
                OtelStatus::InvalidArgument
            );
            // Rejected before the transfer, so the exporter is still ours.
            assert_eq!(
                otel_batch_log_processor_builder_set_exporter(builder, exporter),
                OtelStatus::Ok
            );
            assert!(builder_has_exporter(builder));

            // Matching the trace builder, a second exporter replaces (and frees) the first
            // rather than erroring, so a caller reconfiguring a builder cannot leak.
            let (second, _second_probe) = in_memory_exporter();
            assert_eq!(
                otel_batch_log_processor_builder_set_exporter(builder, second),
                OtelStatus::Ok
            );
            assert!(builder_has_exporter(builder));

            assert_eq!(
                otel_batch_log_processor_builder_set_transport_unknown_probe(builder),
                OtelStatus::Ok
            );
            otel_batch_log_processor_builder_destroy(builder);
        }
    }

    /// Small helper so the ownership test above also exercises a plain numeric setter.
    unsafe fn otel_batch_log_processor_builder_set_transport_unknown_probe(
        builder: *mut OtelBatchLogProcessorBuilder,
    ) -> OtelStatus {
        unsafe { otel_batch_log_processor_builder_set_max_queue_size(builder, 2048) }
    }

    #[test]
    fn batch_builder_rejects_batch_size_larger_than_queue_size_and_stays_reusable() {
        unsafe {
            let builder = otel_batch_log_processor_builder_new();
            let (exporter, _probe) = in_memory_exporter();
            assert_eq!(
                otel_batch_log_processor_builder_set_exporter(builder, exporter),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_batch_log_processor_builder_set_max_queue_size(builder, 8),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_batch_log_processor_builder_set_max_export_batch_size(builder, 16),
                OtelStatus::Ok
            );
            let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_batch_log_processor_builder_build(builder, &mut processor),
                OtelStatus::InvalidConfig
            );
            assert!(processor.is_null());
            // The exporter must NOT have been consumed by the failed build.
            assert!(builder_has_exporter(builder));

            // Fixing the configuration makes the very same builder usable.
            assert_eq!(
                otel_batch_log_processor_builder_set_max_export_batch_size(builder, 4),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_batch_log_processor_builder_set_scheduled_delay_millis(builder, 25),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_batch_log_processor_builder_build(builder, &mut processor),
                OtelStatus::Ok
            );
            assert!(!processor.is_null());
            // Build consumed the exporter; a second build must fail rather than reuse it.
            assert!(!builder_has_exporter(builder));
            let mut second: *mut OtelLogProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_batch_log_processor_builder_build(builder, &mut second),
                OtelStatus::InvalidConfig
            );
            assert!(second.is_null());

            otel_log_processor_destroy(processor);
            otel_batch_log_processor_builder_destroy(builder);
        }
    }

    #[test]
    fn batch_builder_requires_an_exporter() {
        unsafe {
            let builder = otel_batch_log_processor_builder_new();
            let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_batch_log_processor_builder_build(builder, &mut processor),
                OtelStatus::InvalidConfig
            );
            assert!(processor.is_null());
            assert_eq!(
                otel_batch_log_processor_builder_build(builder, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            otel_batch_log_processor_builder_destroy(builder);
        }
    }

    #[test]
    fn null_and_destroyed_handles_are_rejected_by_every_setter() {
        unsafe {
            let null: *mut OtelBatchLogProcessorBuilder = std::ptr::null_mut();
            assert_eq!(
                otel_batch_log_processor_builder_set_max_queue_size(null, 1),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_batch_log_processor_builder_set_max_export_batch_size(null, 1),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_batch_log_processor_builder_set_scheduled_delay_millis(null, 1),
                OtelStatus::InvalidArgument
            );
            // Destroy is a no-op on NULL for both handle kinds.
            otel_batch_log_processor_builder_destroy(null);
            otel_log_processor_destroy(std::ptr::null_mut());
            otel_log_exporter_destroy(std::ptr::null_mut());
        }
    }

    #[test]
    fn simple_processor_exports_records_end_to_end() {
        use opentelemetry::logs::{LogRecord as _, Logger as _, LoggerProvider as _, Severity};
        use opentelemetry::InstrumentationScope;
        use opentelemetry_sdk::logs::SdkLoggerProvider;

        let exporter = InMemoryLogExporter::default();
        let processor = LogProcessorImpl::Simple(Box::new(SimpleLogProcessor::new(
            LogExporterImpl::InMemory(exporter.clone()),
        )));
        let provider = processor.install(SdkLoggerProvider::builder()).build();
        let logger = provider.logger_with_scope(InstrumentationScope::builder("probe").build());
        let mut record = logger.create_log_record();
        record.set_severity_number(Severity::Warn);
        logger.emit(record);

        let emitted = exporter.get_emitted_logs().expect("logs must be readable");
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].record.severity_number(), Some(Severity::Warn));
        let _ = provider.shutdown();
    }
}
