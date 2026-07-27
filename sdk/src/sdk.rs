//! SDK builder and lifecycle.
//!
//! The [`OtelSdkBuilder`] holds resource/service configuration and a list of span processors,
//! then builds an [`OtelSdk`] that owns the resulting `SdkTracerProvider`. Trace exporters and
//! span processors are configured by their own builders ([`crate::otlp_exporter`],
//! [`crate::batch_processor`]) and handed to this builder via `add_span_processor`, so the SDK
//! builder is not coupled to any one exporter or processor implementation.
//!
//! Installing as global (or fetching a provider handle) registers the SDK's implementation
//! into the **API cdylib's** global slot across the C ABI.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;

use opentelemetry_c_abi::{
    OtelHandleHeader, OtelKeyValue, OtelStatus, OtelStringView, OTEL_HANDLE_KIND_SDK,
    OTEL_HANDLE_KIND_SDK_BUILDER,
};

use crate::api_ffi;
use crate::error::{clear_last_error, fail, fail_owned, status_from_export_pipeline_error};
use crate::handle::{
    checked_mut, checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, take,
    HasHandleHeader,
};
use crate::log_processor::{LogProcessorImpl, OtelLogProcessor};
use crate::manual_metric_reader::{OtelManualMetricReader, SharedManualMetricReader};
use crate::metric_view::{MetricViewConfig, OtelMetricView};
#[cfg(feature = "metrics-async-runtime")]
use crate::periodic_metric_reader::AsyncRuntimeGuard;
use crate::periodic_metric_reader::{OtelPeriodicMetricReader, PeriodicMetricReaderImpl};
use crate::span_processor::{OtelSpanProcessor, SpanProcessorImpl};
use crate::vtable;

const MAX_SPAN_PROCESSORS: usize = 64;
const MAX_METRIC_READERS: usize = 64;
const MAX_METRIC_VIEWS: usize = 1024;
const MAX_LOG_PROCESSORS: usize = 64;
const MAX_RESOURCE_ATTRIBUTES: usize = 1024;

/// Opaque builder handle (`otel_sdk_builder_t`). Not thread-safe; confine to one thread.
#[repr(C)]
pub struct OtelSdkBuilder {
    header: OtelHandleHeader,
    service_name: Option<String>,
    resource_attributes: Vec<KeyValue>,
    // Span processors transferred in via `add_span_processor`; moved into the provider on
    // `build`, or freed on destroy if `build` was not completed. Homogeneous `SpanProcessorImpl`
    // so any processor kind (batch today, e.g. simple later) is stored uniformly here.
    processors: Vec<SpanProcessorImpl>,
    metric_readers: Vec<MetricReaderImpl>,
    metric_views: Vec<MetricViewConfig>,
    // Log processors transferred in via `add_log_processor`; moved into the logger provider
    // on `build`, or dropped (and shut down) with the builder if `build` never runs.
    log_processors: Vec<LogProcessorImpl>,
}

enum MetricReaderImpl {
    Periodic(PeriodicMetricReaderImpl),
    Manual(SharedManualMetricReader),
}

impl HasHandleHeader for OtelSdkBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_SDK_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

impl Drop for OtelSdkBuilder {
    fn drop(&mut self) {
        for reader in self.metric_readers.drain(..) {
            match reader {
                MetricReaderImpl::Periodic(reader) => reader.shutdown(),
                MetricReaderImpl::Manual(reader) => reader.shutdown_unregistered(),
            }
        }
    }
}

/// Opaque SDK handle (`otel_sdk_t`). All operations except destroy take shared access.
#[repr(C)]
pub struct OtelSdk {
    header: OtelHandleHeader,
    provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
    shutdown: AtomicBool,
    metrics_lifecycle: Mutex<MetricsLifecycle>,
    logs_lifecycle: Mutex<LogsLifecycle>,
    flush_in_flight: Arc<AtomicBool>,
    #[cfg(feature = "metrics-async-runtime")]
    metric_runtime_guards: Vec<AsyncRuntimeGuard>,
}

#[derive(Default)]
struct MetricsLifecycle {
    shutdown_started: bool,
    global_registration: Option<u64>,
}

/// Mirrors [`MetricsLifecycle`]: Logs owns its own global slot and its own one-shot
/// shutdown, so neither signal can shut down or unregister the other.
#[derive(Default)]
struct LogsLifecycle {
    shutdown_started: bool,
    global_registration: Option<u64>,
}

impl HasHandleHeader for OtelSdk {
    const KIND: u64 = OTEL_HANDLE_KIND_SDK;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

// Concurrent C callers share one SDK by raw pointer; every non-destroy op forms `&OtelSdk`,
// sound across threads only if `OtelSdk: Sync`. Asserted here.
const _: () = {
    fn assert_sync<T: Sync>() {}
    let _ = assert_sync::<OtelSdk>;
};

impl OtelSdk {
    fn metrics_lifecycle(&self) -> std::sync::MutexGuard<'_, MetricsLifecycle> {
        self.metrics_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn logs_lifecycle(&self) -> std::sync::MutexGuard<'_, LogsLifecycle> {
        self.logs_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(feature = "metrics-async-runtime")]
    fn is_current_metric_runtime(&self) -> bool {
        self.metric_runtime_guards
            .iter()
            .any(AsyncRuntimeGuard::is_current)
    }
}

impl Drop for OtelSdk {
    fn drop(&mut self) {
        let registration_id = self
            .metrics_lifecycle
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .global_registration
            .take();
        if let Some(registration_id) = registration_id {
            let _ = api_ffi::unregister_global_meter_provider(registration_id);
        }
        let logs_registration_id = self
            .logs_lifecycle
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .global_registration
            .take();
        if let Some(registration_id) = logs_registration_id {
            let _ = api_ffi::unregister_global_logger_provider(registration_id);
        }
        let _ = self.meter_provider.shutdown();
        // The API's global slot held its own provider reference; it was just released above,
        // so this drop can be the last one. Shut down explicitly rather than relying on the
        // implicit shutdown-on-last-drop, so the outcome is deterministic.
        let _ = self.logger_provider.shutdown();
    }
}

fn optional_millis(millis: u64) -> Option<Duration> {
    (millis != 0).then(|| Duration::from_millis(millis))
}

// ---- Builder lifecycle -----------------------------------------------------

/// Create a new SDK builder with spec-default settings. Release with `otel_sdk_builder_destroy()`.
#[no_mangle]
pub extern "C" fn otel_sdk_builder_new() -> *mut OtelSdkBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelSdkBuilder {
            header: OtelHandleHeader::new(OtelSdkBuilder::KIND),
            service_name: None,
            resource_attributes: Vec::new(),
            processors: Vec::new(),
            metric_readers: Vec::new(),
            metric_views: Vec::new(),
            log_processors: Vec::new(),
        })
    })
}

/// Transfer a Metrics view into an SDK builder. On success the original `view` pointer is
/// invalid and must never be accessed again; on failure it remains caller-owned.
///
/// # Safety
///
/// `builder` and `view` must be live handles and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_metric_view(
    builder: *mut OtelSdkBuilder,
    view: *mut OtelMetricView,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.metric_views.len() >= MAX_METRIC_VIEWS {
            return fail(
                OtelStatus::InvalidConfig,
                "SDK builder Metrics view limit exceeded",
            );
        }
        if builder.metric_views.try_reserve(1).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate space for a Metrics view",
            );
        }
        let view = match unsafe { take(view) } {
            Some(view) => view,
            None => return OtelStatus::InvalidArgument,
        };
        builder.metric_views.push(view.config);
        OtelStatus::Ok
    })
}

/// Transfer a periodic Metrics reader into an SDK builder. On success the SDK builder owns
/// the reader and the original `reader` pointer is invalid; on failure it remains caller-owned.
///
/// # Safety
///
/// `builder` and `reader` must be live handles and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_metric_reader(
    builder: *mut OtelSdkBuilder,
    reader: *mut OtelPeriodicMetricReader,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.metric_readers.len() >= MAX_METRIC_READERS {
            return fail(
                OtelStatus::InvalidConfig,
                "SDK builder Metrics reader limit exceeded",
            );
        }
        if builder.metric_readers.try_reserve(1).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate space for a Metrics reader",
            );
        }
        let reader = match unsafe { take(reader) } {
            Some(reader) => reader,
            None => return OtelStatus::InvalidArgument,
        };
        builder
            .metric_readers
            .push(MetricReaderImpl::Periodic(reader.reader));
        OtelStatus::Ok
    })
}

/// Transfer a manual Metrics reader into an SDK builder. On success the SDK builder owns the
/// reader and the original `reader` pointer is invalid; on failure it remains caller-owned.
/// Application-controlled collection is driven through
/// [`otel_sdk_metrics_force_flush`].
///
/// # Safety
///
/// `builder` and `reader` must be live handles and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_manual_metric_reader(
    builder: *mut OtelSdkBuilder,
    reader: *mut OtelManualMetricReader,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.metric_readers.len() >= MAX_METRIC_READERS {
            return fail(
                OtelStatus::InvalidConfig,
                "SDK builder Metrics reader limit exceeded",
            );
        }
        if builder.metric_readers.try_reserve(1).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate space for a Metrics reader",
            );
        }
        let reader = match unsafe { take(reader) } {
            Some(reader) => reader,
            None => return OtelStatus::InvalidArgument,
        };
        builder
            .metric_readers
            .push(MetricReaderImpl::Manual(reader.reader));
        OtelStatus::Ok
    })
}

/// Destroy an SDK builder (no-op on NULL). Frees any span processors, Metrics readers, and
/// Metrics views transferred in but not yet consumed by `otel_sdk_build`.
///
/// # Safety
/// `builder` must be NULL or a live builder not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_destroy(builder: *mut OtelSdkBuilder) {
    guard_unit(|| unsafe { destroy(builder) });
}

/// # Safety
/// `builder` must satisfy the handle contract (single-threaded).
unsafe fn with_builder<F>(builder: *mut OtelSdkBuilder, f: F) -> OtelStatus
where
    F: FnOnce(&mut OtelSdkBuilder) -> OtelStatus,
{
    guard_status(|| {
        clear_last_error();
        match unsafe { checked_mut(builder) } {
            Some(b) => f(b),
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Set the `service.name` resource attribute.
///
/// # Safety
/// `builder` and `name` must satisfy their contracts.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_set_service_name(
    builder: *mut OtelSdkBuilder,
    name: OtelStringView,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |b| match name.to_string_strict() {
            Ok(name) => {
                b.service_name = Some(name);
                OtelStatus::Ok
            }
            Err(e) => crate::error::fail_abi(e),
        })
    }
}

/// Add an arbitrary resource attribute.
///
/// # Safety
/// `builder` and `attribute` must satisfy their contracts.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_resource_attribute(
    builder: *mut OtelSdkBuilder,
    attribute: OtelKeyValue,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |b| {
            if b.resource_attributes.len() >= MAX_RESOURCE_ATTRIBUTES {
                return fail(
                    OtelStatus::InvalidConfig,
                    "SDK builder resource attribute limit exceeded",
                );
            }
            if b.resource_attributes.try_reserve(1).is_err() {
                return fail(
                    OtelStatus::InternalError,
                    "failed to allocate space for a resource attribute",
                );
            }
            match vtable_to_key_value(&attribute) {
                Ok(kv) => {
                    b.resource_attributes.push(kv);
                    OtelStatus::Ok
                }
                Err(status) => status,
            }
        })
    }
}

/// Add (transfer) a span processor built by a span-processor builder. On `OTEL_STATUS_OK`,
/// ownership of `processor` moves into the SDK builder and the original pointer becomes
/// invalid. On failure (invalid builder or processor), the caller still owns `processor`.
///
/// # Safety
/// `builder` must satisfy the handle contract; `processor` must be NULL or a live
/// `otel_span_processor_t` not used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_span_processor(
    builder: *mut OtelSdkBuilder,
    processor: *mut OtelSpanProcessor,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        // Validate the builder BEFORE taking ownership, so a bad builder leaves the processor
        // caller-owned.
        let builder = match unsafe { checked_mut(builder) } {
            Some(b) => b,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.processors.len() >= MAX_SPAN_PROCESSORS {
            return fail(
                OtelStatus::InvalidConfig,
                "SDK builder span processor limit exceeded",
            );
        }
        if builder.processors.try_reserve(1).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate space for a span processor",
            );
        }
        let owned = match unsafe { take::<OtelSpanProcessor>(processor) } {
            Some(p) => p,
            None => return OtelStatus::InvalidArgument,
        };
        builder.processors.push(owned.processor);
        OtelStatus::Ok
    })
}

/// Add (transfer) a log processor built by a log-processor constructor or builder.
///
/// On `OTEL_STATUS_OK`, ownership of `processor` moves into the SDK builder and the original
/// pointer becomes invalid. On failure (invalid builder or processor, or the per-builder
/// limit being reached) the caller still owns `processor`.
///
/// # Safety
///
/// `builder` must satisfy the handle contract; `processor` must be NULL or a live
/// `otel_log_processor_t` not used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_builder_add_log_processor(
    builder: *mut OtelSdkBuilder,
    processor: *mut OtelLogProcessor,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        // Validate and reserve BEFORE taking ownership, so any rejection leaves the processor
        // caller-owned and destroyable.
        let builder = match unsafe { checked_mut::<OtelSdkBuilder>(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.log_processors.len() >= MAX_LOG_PROCESSORS {
            return fail(
                OtelStatus::InvalidConfig,
                "SDK builder log processor limit exceeded",
            );
        }
        if builder.log_processors.try_reserve(1).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate space for a log processor",
            );
        }
        let owned = match unsafe { take::<OtelLogProcessor>(processor) } {
            Some(owned) => owned,
            None => return OtelStatus::InvalidArgument,
        };
        builder.log_processors.push(owned.processor);
        OtelStatus::Ok
    })
}

// ---- Build -----------------------------------------------------------------

/// Convert a C attribute into an owned `KeyValue` (used for resource attributes).
fn vtable_to_key_value(kv: &OtelKeyValue) -> Result<KeyValue, OtelStatus> {
    // SAFETY: the builder attribute satisfies the OtelKeyValue string contract.
    unsafe { crate::vtable::to_key_value(kv) }
}

fn build_resource(builder: &OtelSdkBuilder) -> Resource {
    let mut resource = Resource::builder();
    if let Some(name) = &builder.service_name {
        resource = resource.with_service_name(name.clone());
    }
    if !builder.resource_attributes.is_empty() {
        resource = resource.with_attributes(builder.resource_attributes.iter().cloned());
    }
    resource.build()
}

/// Build an SDK from the accumulated builder configuration. On `OTEL_STATUS_OK`, `*out_sdk`
/// receives a new [`OtelSdk`] handle owned by the caller. Any span processors added to the
/// builder move into the built SDK. The builder remains owned by the caller (destroy it when
/// done); note that a subsequent build produces an SDK with no processors.
///
/// # Safety
/// `builder` must satisfy the handle contract; `out_sdk` a valid writable `otel_sdk_t*`.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_build(
    builder: *mut OtelSdkBuilder,
    out_sdk: *mut *mut OtelSdk,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out_sdk.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "out_sdk pointer must not be NULL",
            );
        }
        unsafe { *out_sdk = std::ptr::null_mut() };
        let builder = match unsafe { checked_mut(builder) } {
            Some(b) => b,
            None => return OtelStatus::InvalidArgument,
        };
        // Move the transferred processors out of the builder into the provider.
        let processors = std::mem::take(&mut builder.processors);
        let metric_readers = std::mem::take(&mut builder.metric_readers);
        let metric_views = std::mem::take(&mut builder.metric_views);
        let log_processors = std::mem::take(&mut builder.log_processors);
        let resource = build_resource(builder);
        let mut provider_builder = SdkTracerProvider::builder().with_resource(resource);
        for processor in processors {
            provider_builder = provider_builder.with_span_processor(processor);
        }
        let provider = provider_builder.build();
        #[allow(unused_mut)]
        let mut meter_provider_builder =
            SdkMeterProvider::builder().with_resource(build_resource(builder));
        #[cfg(feature = "metrics-async-runtime")]
        let mut metric_runtime_guards = Vec::new();
        for reader in metric_readers {
            match reader {
                MetricReaderImpl::Periodic(PeriodicMetricReaderImpl::Reader(reader)) => {
                    meter_provider_builder = meter_provider_builder.with_reader(reader);
                }
                #[cfg(test)]
                MetricReaderImpl::Periodic(PeriodicMetricReaderImpl::Test { reader, .. }) => {
                    meter_provider_builder = meter_provider_builder.with_reader(reader);
                }
                MetricReaderImpl::Manual(reader) => {
                    meter_provider_builder = meter_provider_builder.with_reader(reader);
                }
                #[cfg(feature = "metrics-async-runtime")]
                MetricReaderImpl::Periodic(PeriodicMetricReaderImpl::Async { reader, runtime }) => {
                    meter_provider_builder = meter_provider_builder.with_reader(reader);
                    metric_runtime_guards.push(runtime);
                }
            }
        }
        for view in metric_views {
            meter_provider_builder =
                meter_provider_builder.with_view(move |instrument| view.apply(instrument));
        }
        let meter_provider = meter_provider_builder.build();
        let mut logger_provider_builder =
            SdkLoggerProvider::builder().with_resource(build_resource(builder));
        for processor in log_processors {
            logger_provider_builder = processor.install(logger_provider_builder);
        }
        let logger_provider = logger_provider_builder.build();
        let sdk = into_raw(OtelSdk {
            header: OtelHandleHeader::new(OtelSdk::KIND),
            provider,
            meter_provider,
            logger_provider,
            shutdown: AtomicBool::new(false),
            metrics_lifecycle: Mutex::new(MetricsLifecycle::default()),
            logs_lifecycle: Mutex::new(LogsLifecycle::default()),
            flush_in_flight: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "metrics-async-runtime")]
            metric_runtime_guards,
        });
        unsafe { *out_sdk = sdk };
        OtelStatus::Ok
    })
}

/// Return an owned API `otel_meter_provider_t*` backed by this SDK.
/// Obtain an API-owned handle for the SDK's MeterProvider.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_get_meter_provider(sdk: *const OtelSdk) -> *mut c_void {
    guard_ptr(|| {
        clear_last_error();
        match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => {
                let ctx = crate::metrics_vtable::provider_ctx(sdk.meter_provider.clone());
                let handle = api_ffi::meter_provider_new(crate::metrics_vtable::vtable_ptr(), ctx);
                if handle.is_null() {
                    (crate::metrics_vtable::SDK_METRICS_VTABLE.provider_free)(ctx);
                }
                handle
            }
            None => std::ptr::null_mut(),
        }
    })
}

/// Install this SDK's Metrics provider into the API-owned global Metrics slot.
/// Install this SDK's MeterProvider in the API-owned global Metrics slot.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_set_metrics_as_global(sdk: *mut OtelSdk) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        let mut lifecycle = sdk.metrics_lifecycle();
        if lifecycle.shutdown_started {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot install a shut-down Metrics provider as global",
            );
        }
        let ctx = crate::metrics_vtable::provider_ctx(sdk.meter_provider.clone());
        let (status, registration_id) =
            api_ffi::register_global_meter_provider(crate::metrics_vtable::vtable_ptr(), ctx);
        if status != OtelStatus::Ok {
            (crate::metrics_vtable::SDK_METRICS_VTABLE.provider_free)(ctx);
        } else if registration_id == 0 {
            return fail(
                OtelStatus::InternalError,
                "Metrics global registration returned an invalid zero token",
            );
        } else {
            lifecycle.global_registration = Some(registration_id);
        }
        status
    })
}

// ---- Provider access and global installation -------------------------------

/// Return an owned tracer-provider handle backed by this SDK. The returned pointer is an
/// API `otel_tracer_provider_t*` (allocated by the API cdylib); release it with
/// `otel_tracer_provider_destroy()`. Returns NULL if `sdk` is invalid.
///
/// # Safety
/// `sdk` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_get_tracer_provider(sdk: *const OtelSdk) -> *mut c_void {
    guard_ptr(|| {
        clear_last_error();
        match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => {
                let ctx = vtable::provider_ctx(sdk.provider.clone());
                let handle = api_ffi::provider_new(vtable::vtable_ptr(), ctx);
                if handle.is_null() {
                    // The API rejected it; free the context we allocated.
                    (vtable::SDK_VTABLE.provider_free)(ctx);
                }
                handle
            }
            None => std::ptr::null_mut(),
        }
    })
}

/// Install this SDK's tracer provider as the process-global provider (in the API-owned
/// slot). May be called more than once; the most recent call wins. Returns
/// `OTEL_STATUS_ALREADY_SHUTDOWN` if the SDK has been shut down.
///
/// # Library lifetime
/// On success this publishes the crate's `'static` vtable and an SDK-owned provider object
/// into the API global slot. Neither [`otel_sdk_shutdown`] nor [`otel_sdk_destroy`] clears
/// that slot; it is cleared only when another provider replaces it. So after a successful
/// install, `libopentelemetry_c_sdk` must remain loaded until process exit or until another
/// provider replaces the slot — shutdown + destroy do **not** make unloading the SDK safe.
///
/// # Safety
/// `sdk` must satisfy the handle contract and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_set_as_global(sdk: *mut OtelSdk) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(s) => s,
            None => return OtelStatus::InvalidArgument,
        };
        if sdk.shutdown.load(Ordering::Acquire) {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot install a shut-down SDK as global",
            );
        }
        let ctx = vtable::provider_ctx(sdk.provider.clone());
        let status = api_ffi::register_global_provider(vtable::vtable_ptr(), ctx);
        if status != OtelStatus::Ok {
            (vtable::SDK_VTABLE.provider_free)(ctx);
        }
        status
    })
}

// ---- Lifecycle -------------------------------------------------------------

fn map_flush_result(result: opentelemetry_sdk::error::OTelSdkResult) -> OtelStatus {
    match result {
        Ok(()) => OtelStatus::Ok,
        Err(err) => status_from_export_pipeline_error(&err),
    }
}

/// Clears the shared force-flush in-flight flag on drop (even if the flush panics).
struct FlushGuard(Arc<AtomicBool>);
impl Drop for FlushGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Flush any buffered spans. `timeout_millis == 0` blocks on the calling thread; otherwise
/// the flush runs on a helper thread (at most one in flight) and returns
/// `OTEL_STATUS_TIMEOUT` if it does not finish in time.
///
/// # Safety
/// `sdk` must satisfy the handle contract and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_force_flush(
    sdk: *mut OtelSdk,
    timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(s) => s,
            None => return OtelStatus::InvalidArgument,
        };
        if sdk.shutdown.load(Ordering::Acquire) {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot force flush a shut-down SDK",
            );
        }
        let timeout = match optional_millis(timeout_millis) {
            None => return map_flush_result(sdk.provider.force_flush()),
            Some(t) => t,
        };
        if sdk.flush_in_flight.swap(true, Ordering::AcqRel) {
            return fail(
                OtelStatus::Timeout,
                "a timed force flush is already in progress; retry after it completes",
            );
        }
        let provider = sdk.provider.clone();
        let guard = FlushGuard(Arc::clone(&sdk.flush_in_flight));
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("otel-c-force-flush".to_owned())
            .spawn(move || {
                let result = provider.force_flush();
                drop(guard);
                let _ = tx.send(result);
            });
        if let Err(err) = spawned {
            sdk.flush_in_flight.store(false, Ordering::Release);
            return fail_owned(
                OtelStatus::InternalError,
                format!("failed to spawn force-flush helper thread: {err}"),
            );
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => map_flush_result(result),
            Err(_) => fail(
                OtelStatus::Timeout,
                "force flush did not complete within the requested timeout",
            ),
        }
    })
}

/// Shut down the SDK, flushing and stopping the pipeline. Runs at most once.
///
/// # Safety
/// `sdk` must satisfy the handle contract and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_shutdown(sdk: *mut OtelSdk, timeout_millis: u64) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(s) => s,
            None => return OtelStatus::InvalidArgument,
        };
        if sdk.shutdown.swap(true, Ordering::AcqRel) {
            return fail(
                OtelStatus::AlreadyShutdown,
                "SDK has already been shut down",
            );
        }

        let timeout = optional_millis(timeout_millis).unwrap_or_else(|| Duration::from_secs(5));
        match sdk.provider.shutdown_with_timeout(timeout) {
            Ok(()) => OtelStatus::Ok,
            Err(err) => status_from_export_pipeline_error(&err),
        }
    })
}

/// Flush all configured Metrics readers. The pinned Rust 0.32 PeriodicReader blocks without
/// accepting a caller timeout, so `timeout_millis` is currently advisory and ignored.
/// Force collection and export through all configured Metrics readers.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_metrics_force_flush(
    sdk: *mut OtelSdk,
    _timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        #[cfg(feature = "metrics-async-runtime")]
        if sdk.is_current_metric_runtime() {
            return fail(
                OtelStatus::InvalidConfig,
                "cannot force flush Metrics reentrantly from an async reader callback",
            );
        }
        if sdk.metrics_lifecycle().shutdown_started {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot force flush a shut-down Metrics provider",
            );
        }
        map_flush_result(sdk.meter_provider.force_flush())
    })
}

/// Shut down the Metrics provider exactly once. OpenTelemetry Rust 0.32 ignores the supplied
/// provider shutdown timeout and PeriodicReader uses its own fixed five-second wait.
/// Shut down the Metrics provider at most once.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_metrics_shutdown(
    sdk: *mut OtelSdk,
    timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        #[cfg(feature = "metrics-async-runtime")]
        if sdk.is_current_metric_runtime() {
            return fail(
                OtelStatus::InvalidConfig,
                "cannot shut down Metrics reentrantly from an async reader callback",
            );
        }
        let registration_id = {
            let mut lifecycle = sdk.metrics_lifecycle();
            if lifecycle.shutdown_started {
                return fail(
                    OtelStatus::AlreadyShutdown,
                    "Metrics provider has already been shut down",
                );
            }
            lifecycle.shutdown_started = true;
            lifecycle.global_registration.take()
        };
        let unregister_status = registration_id
            .map(api_ffi::unregister_global_meter_provider)
            .unwrap_or(OtelStatus::Ok);
        let timeout = optional_millis(timeout_millis).unwrap_or_else(|| Duration::from_secs(5));
        let shutdown_status = match sdk.meter_provider.shutdown_with_timeout(timeout) {
            Ok(()) => OtelStatus::Ok,
            Err(err) => status_from_export_pipeline_error(&err),
        };
        if unregister_status != OtelStatus::Ok {
            unregister_status
        } else {
            shutdown_status
        }
    })
}

// ---- Logs provider access and lifecycle ------------------------------------

/// Obtain an API-owned `otel_logger_provider_t*` backed by this SDK.
///
/// The returned handle is allocated by the API library and must be released with
/// `otel_logger_provider_destroy()`. Returns NULL if `sdk` is invalid.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_get_logger_provider(sdk: *const OtelSdk) -> *mut c_void {
    guard_ptr(|| {
        clear_last_error();
        match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => {
                let ctx = crate::logs_vtable::provider_ctx(sdk.logger_provider.clone());
                let handle = api_ffi::logger_provider_new(crate::logs_vtable::vtable_ptr(), ctx);
                if handle.is_null() {
                    // The API rejected it; release the context we allocated.
                    (crate::logs_vtable::SDK_LOGS_VTABLE.provider_free)(ctx);
                }
                handle
            }
            None => std::ptr::null_mut(),
        }
    })
}

/// Install this SDK's LoggerProvider in the API-owned global Logs slot.
///
/// The Logs slot is independent of the Trace and Metrics slots: installing here neither
/// replaces nor is replaced by the other signals.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_set_logs_as_global(sdk: *mut OtelSdk) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        let mut lifecycle = sdk.logs_lifecycle();
        if lifecycle.shutdown_started {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot install a shut-down LoggerProvider as global",
            );
        }
        let ctx = crate::logs_vtable::provider_ctx(sdk.logger_provider.clone());
        let (status, registration_id) =
            api_ffi::register_global_logger_provider(crate::logs_vtable::vtable_ptr(), ctx);
        if status != OtelStatus::Ok {
            (crate::logs_vtable::SDK_LOGS_VTABLE.provider_free)(ctx);
            return status;
        }
        if registration_id == 0 {
            return fail(
                OtelStatus::InternalError,
                "Logs global registration returned an invalid zero token",
            );
        }
        // Replace, rather than accumulate, this SDK's own registration: only the newest
        // token can still own the slot, so an older one would always be a stale no-op.
        lifecycle.global_registration = Some(registration_id);
        status
    })
}

/// Flush every configured log processor.
///
/// `timeout_millis` is accepted for signature symmetry with the other signals but is
/// **ignored**: the pinned `SdkLoggerProvider::force_flush()` takes no timeout and blocks
/// until each processor finishes. This is documented rather than emulated, because faking a
/// timeout here would return control while the processors were still running.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_logs_force_flush(
    sdk: *mut OtelSdk,
    _timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        if sdk.logs_lifecycle().shutdown_started {
            return fail(
                OtelStatus::AlreadyShutdown,
                "cannot force flush a shut-down LoggerProvider",
            );
        }
        map_flush_result(sdk.logger_provider.force_flush())
    })
}

/// Shut down the LoggerProvider at most once, first clearing the API-owned global slot.
///
/// The pinned provider shutdown is itself one-shot (a repeat returns `AlreadyShutdown`), and
/// this wrapper additionally guarantees the global slot is released exactly once even if two
/// threads race here.
///
/// # Safety
///
/// `sdk` must be a live SDK handle and must not be destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_logs_shutdown(
    sdk: *mut OtelSdk,
    timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let sdk = match unsafe { checked_ref::<OtelSdk>(sdk) } {
            Some(sdk) => sdk,
            None => return OtelStatus::InvalidArgument,
        };
        let registration_id = {
            let mut lifecycle = sdk.logs_lifecycle();
            if lifecycle.shutdown_started {
                return fail(
                    OtelStatus::AlreadyShutdown,
                    "LoggerProvider has already been shut down",
                );
            }
            lifecycle.shutdown_started = true;
            lifecycle.global_registration.take()
        };
        // Unregister first so no new C caller can acquire a logger from a provider that is
        // about to stop accepting records.
        let unregister_status = registration_id
            .map(api_ffi::unregister_global_logger_provider)
            .unwrap_or(OtelStatus::Ok);
        let timeout = optional_millis(timeout_millis).unwrap_or_else(|| Duration::from_secs(5));
        let shutdown_status = match sdk.logger_provider.shutdown_with_timeout(timeout) {
            Ok(()) => OtelStatus::Ok,
            Err(err) => status_from_export_pipeline_error(&err),
        };
        if unregister_status != OtelStatus::Ok {
            unregister_status
        } else {
            shutdown_status
        }
    })
}

/// Destroy an SDK handle (no-op on NULL). Best-effort shutdown on drop.
///
/// # Safety
/// `sdk` must be NULL or a live SDK not used or destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_sdk_destroy(sdk: *mut OtelSdk) {
    guard_unit(|| {
        #[cfg(feature = "metrics-async-runtime")]
        if unsafe { checked_ref(sdk) }.is_some_and(|sdk: &OtelSdk| sdk.is_current_metric_runtime())
        {
            let _ = fail(
                OtelStatus::InvalidConfig,
                "cannot destroy an SDK reentrantly from an async reader callback",
            );
            return;
        }
        unsafe { destroy(sdk) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "otlp-http")]
    use crate::batch_processor::{
        otel_batch_span_processor_builder_build, otel_batch_span_processor_builder_destroy,
        otel_batch_span_processor_builder_new, otel_batch_span_processor_builder_set_exporter,
    };
    use crate::metric_exporter::TestMetricExporterLifecycle;
    #[cfg(feature = "otlp-http")]
    use crate::otlp_exporter::{
        otel_otlp_trace_exporter_builder_build, otel_otlp_trace_exporter_builder_destroy,
        otel_otlp_trace_exporter_builder_new, otel_otlp_trace_exporter_builder_set_endpoint,
    };
    #[cfg(feature = "otlp-http")]
    use crate::span_processor::otel_span_processor_destroy;
    use opentelemetry_c_abi::{OtelAttributeType, OtelAttributeValue};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Barrier, Condvar};

    #[test]
    fn sdk_builder_enforces_resource_and_view_limits_without_consuming_view() {
        unsafe {
            let builder = otel_sdk_builder_new();
            (*builder).resource_attributes =
                vec![KeyValue::new("existing", "value"); MAX_RESOURCE_ATTRIBUTES];
            let key = b"extra";
            let value = b"value";
            let attribute = OtelKeyValue {
                key: OtelStringView {
                    ptr: key.as_ptr().cast(),
                    len: key.len(),
                },
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: OtelStringView {
                        ptr: value.as_ptr().cast(),
                        len: value.len(),
                    },
                },
            };
            assert_eq!(
                otel_sdk_builder_add_resource_attribute(builder, attribute),
                OtelStatus::InvalidConfig
            );

            let view_builder = crate::metric_view::otel_metric_view_builder_new();
            let mut view = std::ptr::null_mut();
            assert_eq!(
                crate::metric_view::otel_metric_view_builder_build(view_builder, &mut view),
                OtelStatus::Ok
            );
            crate::metric_view::otel_metric_view_builder_destroy(view_builder);
            (*builder).metric_views = vec![(*view).config.clone(); MAX_METRIC_VIEWS];
            assert_eq!(
                otel_sdk_builder_add_metric_view(builder, view),
                OtelStatus::InvalidConfig
            );
            crate::metric_view::otel_metric_view_destroy(view);
            otel_sdk_builder_destroy(builder);
        }
    }

    #[cfg(feature = "otlp-http")]
    fn sv(s: &str) -> OtelStringView {
        OtelStringView {
            ptr: s.as_ptr().cast::<std::os::raw::c_char>(),
            len: s.len(),
        }
    }

    /// Build a real (batch + OTLP) span processor via the pipeline builders, for tests that
    /// need a live `otel_span_processor_t`.
    #[cfg(feature = "otlp-http")]
    fn build_processor() -> *mut OtelSpanProcessor {
        unsafe {
            let eb = otel_otlp_trace_exporter_builder_new();
            assert_eq!(
                otel_otlp_trace_exporter_builder_set_endpoint(
                    eb,
                    sv("http://127.0.0.1:9/v1/traces")
                ),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_trace_exporter_builder_build(eb, &mut exporter),
                OtelStatus::Ok
            );
            otel_otlp_trace_exporter_builder_destroy(eb);
            let pb = otel_batch_span_processor_builder_new();
            assert_eq!(
                otel_batch_span_processor_builder_set_exporter(pb, exporter),
                OtelStatus::Ok
            );
            let mut processor = std::ptr::null_mut();
            assert_eq!(
                otel_batch_span_processor_builder_build(pb, &mut processor),
                OtelStatus::Ok
            );
            otel_batch_span_processor_builder_destroy(pb);
            assert!(!processor.is_null());
            processor
        }
    }

    struct LifecycleReader {
        handle: *mut OtelPeriodicMetricReader,
        drops: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
        dropped: Arc<(Mutex<bool>, Condvar)>,
    }

    fn lifecycle_reader() -> LifecycleReader {
        let drops = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new((Mutex::new(false), Condvar::new()));
        let handle = crate::periodic_metric_reader::test_reader_with_lifecycle(
            Arc::clone(&drops),
            TestMetricExporterLifecycle {
                shutdowns: Arc::clone(&shutdowns),
                dropped: Arc::clone(&dropped),
            },
        );
        LifecycleReader {
            handle,
            drops,
            shutdowns,
            dropped,
        }
    }

    fn wait_for_exporter_drop(dropped: &Arc<(Mutex<bool>, Condvar)>) {
        let (dropped, condition) = &**dropped;
        let guard = dropped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = condition
            .wait_timeout_while(guard, Duration::from_secs(5), |dropped| !*dropped)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            *guard,
            "exporter was not dropped before the bounded deadline"
        );
    }

    #[test]
    fn metric_reader_transfer_and_sdk_destruction_release_exporter_once() {
        unsafe {
            let LifecycleReader {
                handle: reader,
                drops,
                shutdowns,
                dropped,
            } = lifecycle_reader();
            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_metric_reader(builder, reader),
                OtelStatus::Ok
            );
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 0);

            let mut sdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(builder);
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 0);
            otel_sdk_destroy(sdk);
            assert_eq!(shutdowns.load(AtomicOrdering::SeqCst), 1);
            wait_for_exporter_drop(&dropped);
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
        }
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn async_reader_lifecycle_calls_fail_closed_on_owned_runtime() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let reader = crate::periodic_metric_reader::test_async_reader(Arc::clone(&drops));
            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_metric_reader(builder, reader),
                OtelStatus::Ok
            );
            let mut sdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(builder);
            let runtime = (&(*sdk).metric_runtime_guards)[0].handle();
            runtime.block_on(async {
                assert_eq!(
                    otel_sdk_metrics_force_flush(sdk, 0),
                    OtelStatus::InvalidConfig
                );
                assert!(crate::api_ffi::test_probe::last_error()
                    .contains("reentrantly from an async reader callback"));
                assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::InvalidConfig);
                otel_sdk_destroy(sdk);
                assert!(crate::api_ffi::test_probe::last_error()
                    .contains("cannot destroy an SDK reentrantly"));
            });
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 0);
            drop(runtime);
            otel_sdk_destroy(sdk);
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
        }
    }

    #[test]
    fn failed_sdk_build_and_invalid_transfer_preserve_reader_ownership() {
        unsafe {
            let LifecycleReader {
                handle: invalid_reader,
                drops: invalid_drops,
                shutdowns: invalid_shutdowns,
                dropped: invalid_dropped,
            } = lifecycle_reader();
            assert_eq!(
                otel_sdk_builder_add_metric_reader(std::ptr::null_mut(), invalid_reader),
                OtelStatus::InvalidArgument
            );
            assert_eq!(invalid_drops.load(AtomicOrdering::SeqCst), 0);
            crate::periodic_metric_reader::otel_periodic_metric_reader_destroy(invalid_reader);
            assert_eq!(invalid_shutdowns.load(AtomicOrdering::SeqCst), 1);
            wait_for_exporter_drop(&invalid_dropped);
            assert_eq!(invalid_drops.load(AtomicOrdering::SeqCst), 1);

            let LifecycleReader {
                handle: reader,
                drops: build_drops,
                shutdowns: build_shutdowns,
                dropped: build_dropped,
            } = lifecycle_reader();
            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_metric_reader(builder, reader),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_sdk_build(builder, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            assert_eq!(build_drops.load(AtomicOrdering::SeqCst), 0);
            otel_sdk_builder_destroy(builder);
            assert_eq!(build_shutdowns.load(AtomicOrdering::SeqCst), 1);
            wait_for_exporter_drop(&build_dropped);
            assert_eq!(build_drops.load(AtomicOrdering::SeqCst), 1);
        }
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn set_as_global_registers_sdk_vtable_with_api() {
        // Prove the SDK installs *its* vtable + a non-null provider context into the API's
        // registration ABI (stubbed in unit tests; exercised for real by the cross-artifact
        // C test).
        unsafe {
            let processor = build_processor();
            let b = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_set_service_name(b, sv("unit-test")),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_sdk_builder_add_span_processor(b, processor),
                OtelStatus::Ok
            );
            let mut sdk: *mut OtelSdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(b, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(b);

            assert_eq!(otel_sdk_set_as_global(sdk), OtelStatus::Ok);
            let (vtable, ctx) =
                crate::api_ffi::test_probe::registered().expect("SDK must register a provider");
            assert_eq!(vtable, crate::vtable::vtable_ptr());
            assert!(!ctx.is_null());
            // Free the context we handed to the (stub) API to avoid a leak in the test.
            (crate::vtable::SDK_VTABLE.provider_free)(ctx);

            otel_sdk_shutdown(sdk, 500);
            otel_sdk_destroy(sdk);
        }
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn add_span_processor_ownership_transfer() {
        unsafe {
            // Failure: a bad (NULL) builder leaves the processor caller-owned, so we can still
            // destroy it without a leak/double-free.
            let processor = build_processor();
            assert_eq!(
                otel_sdk_builder_add_span_processor(std::ptr::null_mut(), processor),
                OtelStatus::InvalidArgument
            );
            otel_span_processor_destroy(processor); // still owned by caller: frees it

            // Success: ownership transfers into the SDK builder and invalidates the original
            // pointer. Destroying the builder frees the processor exactly once.
            let processor = build_processor();
            let b = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_span_processor(b, processor),
                OtelStatus::Ok
            );
            otel_sdk_builder_destroy(b); // frees the transferred processor
        }
    }

    #[test]
    fn build_with_no_processor_succeeds() {
        // A provider with no span processor is valid (spans are simply not exported).
        unsafe {
            let b = otel_sdk_builder_new();
            let mut sdk: *mut OtelSdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(b, &mut sdk), OtelStatus::Ok);
            assert!(!sdk.is_null());
            otel_sdk_builder_destroy(b);
            otel_sdk_shutdown(sdk, 500);
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn metrics_shutdown_removes_its_global_registration() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let builder = otel_sdk_builder_new();
            let mut sdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(builder);
            assert_eq!(otel_sdk_set_metrics_as_global(sdk), OtelStatus::Ok);
            assert!(crate::api_ffi::test_probe::metrics_registered());
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(sdk);
        }
    }

    unsafe fn build_test_sdk() -> *mut OtelSdk {
        let builder = otel_sdk_builder_new();
        let mut sdk = std::ptr::null_mut();
        assert_eq!(unsafe { otel_sdk_build(builder, &mut sdk) }, OtelStatus::Ok);
        unsafe { otel_sdk_builder_destroy(builder) };
        sdk
    }

    #[test]
    fn metrics_shutdown_prevents_later_installation() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let sdk = build_test_sdk();
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::Ok);
            assert_eq!(
                otel_sdk_set_metrics_as_global(sdk),
                OtelStatus::AlreadyShutdown
            );
            assert!(!crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn metrics_flush_and_shutdown_statuses_are_stable() {
        unsafe {
            assert_eq!(
                otel_sdk_metrics_force_flush(std::ptr::null_mut(), 0),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_sdk_metrics_shutdown(std::ptr::null_mut(), 0),
                OtelStatus::InvalidArgument
            );

            let dead = build_test_sdk();
            (*dead).header.poison();
            assert_eq!(
                otel_sdk_metrics_force_flush(dead, 0),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_sdk_metrics_shutdown(dead, 0),
                OtelStatus::InvalidArgument
            );
            (*dead).header = OtelHandleHeader::new(OtelSdk::KIND);
            otel_sdk_destroy(dead);

            let sdk = build_test_sdk();
            assert_eq!(otel_sdk_metrics_force_flush(sdk, 0), OtelStatus::Ok);
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::Ok);
            assert_eq!(
                otel_sdk_metrics_force_flush(sdk, 0),
                OtelStatus::AlreadyShutdown
            );
            assert_eq!(
                otel_sdk_metrics_shutdown(sdk, 0),
                OtelStatus::AlreadyShutdown
            );
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn explicit_metrics_shutdown_does_not_release_pipeline_twice() {
        unsafe {
            let LifecycleReader {
                handle: reader,
                drops,
                shutdowns,
                dropped,
            } = lifecycle_reader();
            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_metric_reader(builder, reader),
                OtelStatus::Ok
            );
            let mut sdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(builder);

            assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::Ok);
            assert_eq!(shutdowns.load(AtomicOrdering::SeqCst), 1);
            assert_eq!(
                otel_sdk_metrics_shutdown(sdk, 0),
                OtelStatus::AlreadyShutdown
            );
            assert_eq!(shutdowns.load(AtomicOrdering::SeqCst), 1);
            otel_sdk_destroy(sdk);
            assert_eq!(shutdowns.load(AtomicOrdering::SeqCst), 1);
            wait_for_exporter_drop(&dropped);
            assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
        }
    }

    #[test]
    fn concurrent_metrics_install_and_shutdown_leave_no_registration() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for _ in 0..32 {
            unsafe {
                let sdk = build_test_sdk();
                let barrier = Arc::new(Barrier::new(3));
                let install_barrier = Arc::clone(&barrier);
                let shutdown_barrier = Arc::clone(&barrier);
                let sdk_addr = sdk as usize;
                let install = std::thread::spawn(move || {
                    install_barrier.wait();
                    otel_sdk_set_metrics_as_global(sdk_addr as *mut OtelSdk)
                });
                let shutdown = std::thread::spawn(move || {
                    shutdown_barrier.wait();
                    otel_sdk_metrics_shutdown(sdk_addr as *mut OtelSdk, 0)
                });
                barrier.wait();
                let install_status = install.join().unwrap();
                assert!(matches!(
                    install_status,
                    OtelStatus::Ok | OtelStatus::AlreadyShutdown
                ));
                assert_eq!(shutdown.join().unwrap(), OtelStatus::Ok);
                assert!(!crate::api_ffi::test_probe::metrics_registered());
                otel_sdk_destroy(sdk);
            }
        }
    }

    #[test]
    fn concurrent_same_sdk_installs_track_the_published_token() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let sdk = build_test_sdk();
            let barrier = Arc::new(Barrier::new(3));
            let first_barrier = Arc::clone(&barrier);
            let second_barrier = Arc::clone(&barrier);
            let sdk_addr = sdk as usize;
            let first = std::thread::spawn(move || {
                first_barrier.wait();
                otel_sdk_set_metrics_as_global(sdk_addr as *mut OtelSdk)
            });
            let second = std::thread::spawn(move || {
                second_barrier.wait();
                otel_sdk_set_metrics_as_global(sdk_addr as *mut OtelSdk)
            });
            barrier.wait();
            assert_eq!(first.join().unwrap(), OtelStatus::Ok);
            assert_eq!(second.join().unwrap(), OtelStatus::Ok);
            let published = crate::api_ffi::test_probe::metrics_registration_id().unwrap();
            assert_eq!(
                (*sdk).metrics_lifecycle().global_registration,
                Some(published)
            );
            assert_eq!(otel_sdk_metrics_shutdown(sdk, 0), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn older_sdk_shutdown_cannot_clear_newer_registration() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let older = build_test_sdk();
            let newer = build_test_sdk();
            assert_eq!(otel_sdk_set_metrics_as_global(older), OtelStatus::Ok);
            assert_eq!(otel_sdk_set_metrics_as_global(newer), OtelStatus::Ok);
            let newer_id = crate::api_ffi::test_probe::metrics_registration_id();
            assert_eq!(otel_sdk_metrics_shutdown(older, 0), OtelStatus::Ok);
            assert_eq!(
                crate::api_ffi::test_probe::metrics_registration_id(),
                newer_id
            );
            assert_eq!(otel_sdk_metrics_shutdown(newer, 0), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(older);
            otel_sdk_destroy(newer);
        }
    }

    #[test]
    fn repeated_install_and_destroy_without_shutdown_are_safe() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let sdk = build_test_sdk();
            assert_eq!(otel_sdk_set_metrics_as_global(sdk), OtelStatus::Ok);
            assert_eq!(otel_sdk_set_metrics_as_global(sdk), OtelStatus::Ok);
            assert!(crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(sdk);
            assert!(!crate::api_ffi::test_probe::metrics_registered());
        }
    }

    // ---- Logs lifecycle ----------------------------------------------------

    /// Build an SDK whose logger provider exports into an in-memory exporter, returning both
    /// so tests can assert on what actually reached the pipeline.
    unsafe fn sdk_with_in_memory_logs(
    ) -> (*mut OtelSdk, opentelemetry_sdk::logs::InMemoryLogExporter) {
        unsafe {
            use crate::log_exporter::{LogExporterImpl, OtelLogExporter};
            use crate::log_processor::{otel_simple_log_processor_create, OtelLogProcessor};

            let exporter = opentelemetry_sdk::logs::InMemoryLogExporter::default();
            let exporter_handle = crate::handle::into_raw(OtelLogExporter::new(
                LogExporterImpl::InMemory(exporter.clone()),
            ));
            let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_simple_log_processor_create(exporter_handle, &mut processor),
                OtelStatus::Ok
            );
            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_log_processor(builder, processor),
                OtelStatus::Ok
            );
            let mut sdk: *mut OtelSdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            otel_sdk_builder_destroy(builder);
            (sdk, exporter)
        }
    }

    #[test]
    fn add_log_processor_transfers_ownership_only_on_success() {
        use crate::log_exporter::{LogExporterImpl, OtelLogExporter};
        use crate::log_processor::{
            otel_log_processor_destroy, otel_simple_log_processor_create, OtelLogProcessor,
        };

        unsafe {
            let make_processor = || {
                let exporter =
                    crate::handle::into_raw(OtelLogExporter::new(LogExporterImpl::InMemory(
                        opentelemetry_sdk::logs::InMemoryLogExporter::default(),
                    )));
                let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
                assert_eq!(
                    otel_simple_log_processor_create(exporter, &mut processor),
                    OtelStatus::Ok
                );
                processor
            };

            // A rejected call must leave the processor caller-owned so it can still be freed.
            let processor = make_processor();
            assert_eq!(
                otel_sdk_builder_add_log_processor(std::ptr::null_mut(), processor),
                OtelStatus::InvalidArgument
            );
            otel_log_processor_destroy(processor);

            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_log_processor(builder, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );

            // Success transfers ownership; destroying the builder releases it exactly once.
            let processor = make_processor();
            assert_eq!(
                otel_sdk_builder_add_log_processor(builder, processor),
                OtelStatus::Ok
            );
            assert_eq!((*builder).log_processors.len(), 1);
            otel_sdk_builder_destroy(builder);
        }
    }

    #[test]
    fn add_log_processor_enforces_its_limit_without_consuming_the_processor() {
        use crate::log_exporter::{LogExporterImpl, OtelLogExporter};
        use crate::log_processor::{
            otel_log_processor_destroy, otel_simple_log_processor_create, OtelLogProcessor,
        };

        unsafe {
            let builder = otel_sdk_builder_new();
            // Fill the vector directly: constructing 64 real processors would be wasteful and
            // the limit check runs before ownership is taken either way.
            for _ in 0..MAX_LOG_PROCESSORS {
                (*builder)
                    .log_processors
                    .push(LogProcessorImpl::Simple(Box::new(
                        opentelemetry_sdk::logs::SimpleLogProcessor::new(
                            LogExporterImpl::InMemory(
                                opentelemetry_sdk::logs::InMemoryLogExporter::default(),
                            ),
                        ),
                    )));
            }
            let exporter = crate::handle::into_raw(OtelLogExporter::new(
                LogExporterImpl::InMemory(opentelemetry_sdk::logs::InMemoryLogExporter::default()),
            ));
            let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_simple_log_processor_create(exporter, &mut processor),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_sdk_builder_add_log_processor(builder, processor),
                OtelStatus::InvalidConfig
            );
            // Still caller-owned after the rejection.
            otel_log_processor_destroy(processor);
            otel_sdk_builder_destroy(builder);
        }
    }

    #[test]
    fn logs_pipeline_receives_records_emitted_through_the_sdk_logger_provider() {
        use opentelemetry::logs::{LogRecord as _, Logger as _, LoggerProvider as _, Severity};
        use opentelemetry::InstrumentationScope;

        unsafe {
            let (sdk, exporter) = sdk_with_in_memory_logs();
            let provider = (*sdk).logger_provider.clone();
            let logger = provider.logger_with_scope(InstrumentationScope::builder("scope").build());
            let mut record = logger.create_log_record();
            record.set_severity_number(Severity::Error);
            logger.emit(record);

            // Read BEFORE shutdown: the in-memory exporter clears its buffer on shutdown.
            let emitted = exporter.get_emitted_logs().expect("readable");
            assert_eq!(emitted.len(), 1);
            assert_eq!(emitted[0].record.severity_number(), Some(Severity::Error));
            drop(provider);

            assert_eq!(otel_sdk_logs_force_flush(sdk, 500), OtelStatus::Ok);
            assert_eq!(otel_sdk_logs_shutdown(sdk, 500), OtelStatus::Ok);
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn logs_entry_points_reject_invalid_sdk_handles() {
        unsafe {
            assert!(otel_sdk_get_logger_provider(std::ptr::null()).is_null());
            assert_eq!(
                otel_sdk_set_logs_as_global(std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_sdk_logs_force_flush(std::ptr::null_mut(), 0),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_sdk_logs_shutdown(std::ptr::null_mut(), 0),
                OtelStatus::InvalidArgument
            );
        }
    }

    #[test]
    fn logs_shutdown_is_one_shot_and_blocks_later_installation_and_flush() {
        let _guard = crate::api_ffi::test_probe::LOGS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let (sdk, _exporter) = sdk_with_in_memory_logs();
            assert_eq!(otel_sdk_logs_shutdown(sdk, 500), OtelStatus::Ok);
            assert_eq!(
                otel_sdk_logs_shutdown(sdk, 500),
                OtelStatus::AlreadyShutdown
            );
            assert_eq!(
                otel_sdk_set_logs_as_global(sdk),
                OtelStatus::AlreadyShutdown
            );
            assert_eq!(
                otel_sdk_logs_force_flush(sdk, 500),
                OtelStatus::AlreadyShutdown
            );
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn logs_global_installation_registers_the_logs_vtable_and_shutdown_clears_it() {
        let _guard = crate::api_ffi::test_probe::LOGS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let (sdk, _exporter) = sdk_with_in_memory_logs();
            assert_eq!(otel_sdk_set_logs_as_global(sdk), OtelStatus::Ok);
            assert!(crate::api_ffi::test_probe::logs_registered());
            let first = crate::api_ffi::test_probe::logs_registration_id()
                .expect("a non-zero registration token must be published");

            // Re-installing replaces this SDK's own token rather than accumulating tokens.
            assert_eq!(otel_sdk_set_logs_as_global(sdk), OtelStatus::Ok);
            let second = crate::api_ffi::test_probe::logs_registration_id().expect("token");
            assert_ne!(first, second);

            assert_eq!(otel_sdk_logs_shutdown(sdk, 500), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::logs_registered());
            otel_sdk_destroy(sdk);
        }
    }

    #[test]
    fn dropping_the_sdk_without_explicit_shutdown_clears_the_logs_global_slot() {
        let _guard = crate::api_ffi::test_probe::LOGS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let (sdk, _exporter) = sdk_with_in_memory_logs();
            assert_eq!(otel_sdk_set_logs_as_global(sdk), OtelStatus::Ok);
            assert!(crate::api_ffi::test_probe::logs_registered());
            // No explicit `otel_sdk_logs_shutdown`: Drop must still unregister.
            otel_sdk_destroy(sdk);
            assert!(!crate::api_ffi::test_probe::logs_registered());
        }
    }

    #[test]
    fn logs_and_metrics_global_slots_are_independent() {
        let _logs_guard = crate::api_ffi::test_probe::LOGS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _metrics_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let (sdk, _exporter) = sdk_with_in_memory_logs();
            assert_eq!(otel_sdk_set_as_global_metrics_probe(sdk), OtelStatus::Ok);
            assert_eq!(otel_sdk_set_logs_as_global(sdk), OtelStatus::Ok);

            // Shutting down Logs must not disturb the Metrics registration, and vice versa.
            assert_eq!(otel_sdk_logs_shutdown(sdk, 500), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::logs_registered());
            assert!(crate::api_ffi::test_probe::metrics_registered());

            assert_eq!(otel_sdk_metrics_shutdown(sdk, 500), OtelStatus::Ok);
            assert!(!crate::api_ffi::test_probe::metrics_registered());
            otel_sdk_destroy(sdk);
        }
    }

    unsafe fn otel_sdk_set_as_global_metrics_probe(sdk: *mut OtelSdk) -> OtelStatus {
        unsafe { otel_sdk_set_metrics_as_global(sdk) }
    }

    #[test]
    fn concurrent_logs_install_and_shutdown_leave_no_registration() {
        let _guard = crate::api_ffi::test_probe::LOGS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let (sdk, _exporter) = sdk_with_in_memory_logs();
            let address = sdk as usize;
            let barrier = Arc::new(Barrier::new(2));
            let installer = {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    otel_sdk_set_logs_as_global(address as *mut OtelSdk)
                })
            };
            barrier.wait();
            let shutdown = otel_sdk_logs_shutdown(sdk, 500);
            let install = installer.join().expect("installer thread must not panic");

            // Whatever the interleaving, shutdown wins: the slot must end up empty and the
            // install must either have been refused or already been undone.
            assert!(matches!(shutdown, OtelStatus::Ok));
            assert!(matches!(
                install,
                OtelStatus::Ok | OtelStatus::AlreadyShutdown
            ));
            if install == OtelStatus::Ok {
                // The install landed first; a second shutdown is a no-op, so clean up the slot.
                let _ = crate::api_ffi::test_probe::logs_registration_id()
                    .map(api_ffi::unregister_global_logger_provider);
            }
            assert!(!crate::api_ffi::test_probe::logs_registered());
            otel_sdk_destroy(sdk);
        }
    }

    /// Emission and shutdown race by construction: a C caller can hold a logger handle across a
    /// shutdown on another thread, and the pinned `SdkLoggerProvider` answers that by turning
    /// its loggers into no-ops rather than by invalidating them. This drives that window hard
    /// and asserts the only two properties we can actually promise: every emit returns a
    /// defined status without unwinding across the ABI, and the export buffer never sees more
    /// records than were emitted.
    #[test]
    fn concurrent_emit_during_logs_shutdown_stays_defined() {
        const THREADS: usize = 4;
        const EMITS_PER_THREAD: usize = 500;

        use opentelemetry_c_abi::{OtelLogRecordView, OtelScopeConfig};

        unsafe {
            let (sdk, exporter) = sdk_with_in_memory_logs();
            let barrier = Arc::new(Barrier::new(THREADS + 1));
            let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));

            let mut emitters = Vec::new();
            for _ in 0..THREADS {
                // Each thread takes its own owned provider reference through the vtable, exactly
                // as an independent C caller would, so no Rust-side sharing is assumed.
                let ctx = crate::logs_vtable::provider_ctx((*sdk).logger_provider.clone()) as usize;
                let barrier = Arc::clone(&barrier);
                let accepted = Arc::clone(&accepted);
                emitters.push(std::thread::spawn(move || {
                    let vtable = crate::logs_vtable::vtable_ptr();
                    let ctx = ctx as *mut std::ffi::c_void;
                    const SCOPE_NAME: &str = "stress";
                    let scope = OtelScopeConfig {
                        name: OtelStringView {
                            ptr: SCOPE_NAME.as_ptr().cast::<std::ffi::c_char>(),
                            len: SCOPE_NAME.len(),
                        },
                        version: OtelStringView::empty(),
                        schema_url: OtelStringView::empty(),
                        attributes: std::ptr::null(),
                        attribute_count: 0,
                    };
                    let logger = ((*vtable).provider_get_logger)(ctx, &scope);
                    let mut record: OtelLogRecordView = std::mem::zeroed();
                    record.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64;
                    record.severity_number = 9;

                    barrier.wait();
                    for _ in 0..EMITS_PER_THREAD {
                        let status = ((*vtable).logger_emit)(logger, &record);
                        match status {
                            OtelStatus::Ok => {
                                accepted.fetch_add(1, Ordering::Relaxed);
                            }
                            // A shut-down provider must degrade, not fail loudly or corrupt.
                            other => assert_eq!(other, OtelStatus::AlreadyShutdown),
                        }
                        let _ = ((*vtable).logger_enabled)(logger, 9);
                    }
                    ((*vtable).logger_free)(logger);
                    ((*vtable).provider_free)(ctx);
                }));
            }

            barrier.wait();
            // Land the shutdown in the middle of the emit storm rather than after it.
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert_eq!(otel_sdk_logs_shutdown(sdk, 1_000), OtelStatus::Ok);

            for emitter in emitters {
                emitter.join().expect("emitter thread must not panic");
            }

            let accepted = accepted.load(Ordering::Relaxed);
            assert!(accepted > 0, "the race left no emit path exercised");
            assert!(accepted <= THREADS * EMITS_PER_THREAD);
            // The in-memory exporter clears on shutdown, so this is an upper-bound check on the
            // records that survived: it must never exceed what was accepted.
            let exported = exporter
                .get_emitted_logs()
                .map(|logs| logs.len())
                .unwrap_or(0);
            assert!(
                exported <= accepted,
                "exported {exported} records but only {accepted} emits were accepted"
            );

            // Shutdown is one-shot regardless of the racing traffic.
            assert_eq!(otel_sdk_logs_shutdown(sdk, 0), OtelStatus::AlreadyShutdown);
            otel_sdk_destroy(sdk);
        }
    }

    /// Batch queue saturation crossed with repeated pipeline creation and destruction.
    ///
    /// These are combined on purpose. Saturation is what makes the drop path hot, and repeated
    /// create/destroy is what would expose a leak or a stale registration in that path; running
    /// either alone leaves the other's failure mode untested. The queue is deliberately far
    /// smaller than the emit count so records are dropped continuously while flush and shutdown
    /// run against the same processor.
    #[test]
    fn saturated_batch_queue_survives_repeated_pipeline_lifecycles() {
        use crate::log_exporter::{LogExporterImpl, OtelLogExporter};
        use crate::log_processor::{
            otel_batch_log_processor_builder_build, otel_batch_log_processor_builder_destroy,
            otel_batch_log_processor_builder_new, otel_batch_log_processor_builder_set_exporter,
            otel_batch_log_processor_builder_set_max_export_batch_size,
            otel_batch_log_processor_builder_set_max_queue_size,
            otel_batch_log_processor_builder_set_scheduled_delay_millis, OtelLogProcessor,
        };
        use opentelemetry_c_abi::{OtelLogRecordView, OtelScopeConfig};

        const CYCLES: usize = 8;
        const EMIT_THREADS: usize = 3;
        const EMITS_PER_THREAD: usize = 400;
        // Far below the emit count, so the queue is saturated for nearly the whole run.
        const QUEUE_SIZE: usize = 8;

        for cycle in 0..CYCLES {
            unsafe {
                let exporter = opentelemetry_sdk::logs::InMemoryLogExporter::default();
                let exporter_handle = crate::handle::into_raw(OtelLogExporter::new(
                    LogExporterImpl::InMemory(exporter.clone()),
                ));
                let processor_builder = otel_batch_log_processor_builder_new();
                assert_eq!(
                    otel_batch_log_processor_builder_set_exporter(
                        processor_builder,
                        exporter_handle
                    ),
                    OtelStatus::Ok,
                    "cycle {cycle}"
                );
                assert_eq!(
                    otel_batch_log_processor_builder_set_max_queue_size(
                        processor_builder,
                        QUEUE_SIZE
                    ),
                    OtelStatus::Ok
                );
                assert_eq!(
                    otel_batch_log_processor_builder_set_max_export_batch_size(
                        processor_builder,
                        QUEUE_SIZE
                    ),
                    OtelStatus::Ok
                );
                assert_eq!(
                    otel_batch_log_processor_builder_set_scheduled_delay_millis(
                        processor_builder,
                        1
                    ),
                    OtelStatus::Ok
                );
                let mut processor: *mut OtelLogProcessor = std::ptr::null_mut();
                assert_eq!(
                    otel_batch_log_processor_builder_build(processor_builder, &mut processor),
                    OtelStatus::Ok
                );
                otel_batch_log_processor_builder_destroy(processor_builder);

                let builder = otel_sdk_builder_new();
                assert_eq!(
                    otel_sdk_builder_add_log_processor(builder, processor),
                    OtelStatus::Ok
                );
                let mut sdk: *mut OtelSdk = std::ptr::null_mut();
                assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
                otel_sdk_builder_destroy(builder);

                let barrier = Arc::new(Barrier::new(EMIT_THREADS + 1));
                let mut emitters = Vec::new();
                for _ in 0..EMIT_THREADS {
                    let ctx =
                        crate::logs_vtable::provider_ctx((*sdk).logger_provider.clone()) as usize;
                    let barrier = Arc::clone(&barrier);
                    emitters.push(std::thread::spawn(move || {
                        let vtable = crate::logs_vtable::vtable_ptr();
                        let ctx = ctx as *mut std::ffi::c_void;
                        const SCOPE_NAME: &str = "saturation";
                        let scope = OtelScopeConfig {
                            name: OtelStringView {
                                ptr: SCOPE_NAME.as_ptr().cast::<std::ffi::c_char>(),
                                len: SCOPE_NAME.len(),
                            },
                            version: OtelStringView::empty(),
                            schema_url: OtelStringView::empty(),
                            attributes: std::ptr::null(),
                            attribute_count: 0,
                        };
                        let logger = ((*vtable).provider_get_logger)(ctx, &scope);
                        let mut record: OtelLogRecordView = std::mem::zeroed();
                        record.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64;
                        record.severity_number = 9;
                        barrier.wait();
                        for _ in 0..EMITS_PER_THREAD {
                            let status = ((*vtable).logger_emit)(logger, &record);
                            assert!(
                                matches!(status, OtelStatus::Ok | OtelStatus::AlreadyShutdown),
                                "unexpected emit status {status:?}"
                            );
                        }
                        ((*vtable).logger_free)(logger);
                        ((*vtable).provider_free)(ctx);
                    }));
                }

                barrier.wait();
                // Flush against a saturated queue while emitters are still running.
                let flush = otel_sdk_logs_force_flush(sdk, 1_000);
                assert!(
                    matches!(flush, OtelStatus::Ok | OtelStatus::AlreadyShutdown),
                    "cycle {cycle}: unexpected flush status {flush:?}"
                );

                for emitter in emitters {
                    emitter.join().expect("emitter thread must not panic");
                }

                assert_eq!(otel_sdk_logs_shutdown(sdk, 1_000), OtelStatus::Ok);
                assert_eq!(
                    otel_sdk_logs_shutdown(sdk, 0),
                    OtelStatus::AlreadyShutdown,
                    "cycle {cycle}: shutdown must stay one-shot under saturation"
                );
                otel_sdk_destroy(sdk);
            }
        }
    }

    #[test]
    fn flush_guard_clears_on_panic() {
        let flag = Arc::new(AtomicBool::new(true));
        let inner = Arc::clone(&flag);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = FlushGuard(inner);
            panic!("boom (expected)");
        }));
        assert!(r.is_err());
        assert!(!flag.load(Ordering::Acquire));
    }
}
