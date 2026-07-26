//! Periodic Metrics reader builder and transferable reader handle.

use std::time::Duration;

use opentelemetry_c_abi::{
    OtelHandleHeader, OtelStatus, OTEL_HANDLE_KIND_PERIODIC_METRIC_READER,
    OTEL_HANDLE_KIND_PERIODIC_METRIC_READER_BUILDER,
};

#[cfg(feature = "metrics-async-runtime")]
use crate::error::fail_owned;
use crate::error::{clear_last_error, fail};
#[cfg(feature = "metrics-async-runtime")]
use crate::handle::checked_ref;
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, take, HasHandleHeader,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

use opentelemetry_sdk::metrics::PeriodicReaderBuilder;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
#[cfg(feature = "metrics-async-runtime")]
use opentelemetry_sdk::runtime::Runtime;

const READER_RUNTIME_BLOCKING: u32 = 0;
const READER_RUNTIME_ASYNC: u32 = 1;

#[cfg(feature = "metrics-async-runtime")]
type AsyncPeriodicReader =
    opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader<
        MetricExporterImpl,
    >;

pub(crate) enum PeriodicMetricReaderImpl {
    Reader(PeriodicReader<MetricExporterImpl>),
    #[cfg(feature = "metrics-async-runtime")]
    Async {
        reader: AsyncPeriodicReader,
        runtime: AsyncRuntimeGuard,
    },
    #[cfg(test)]
    Test {
        reader: PeriodicReader<MetricExporterImpl>,
        #[allow(dead_code)]
        configured_interval: Option<Duration>,
    },
}

impl PeriodicMetricReaderImpl {
    #[cfg(feature = "metrics-async-runtime")]
    pub(crate) fn is_current_async_runtime(&self) -> bool {
        match self {
            Self::Async { runtime, .. } => runtime.is_current(),
            _ => false,
        }
    }

    pub(crate) fn shutdown(self) {
        match self {
            Self::Reader(reader) => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
            }
            #[cfg(feature = "metrics-async-runtime")]
            Self::Async { reader, runtime } => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
                drop(provider);
                drop(runtime);
            }
            #[cfg(test)]
            Self::Test { reader, .. } => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
            }
        }
    }
}

#[repr(C)]
pub struct OtelPeriodicMetricReaderBuilder {
    header: OtelHandleHeader,
    interval: Option<Duration>,
    timeout: Option<Duration>,
    runtime: u32,
    exporter: Option<MetricExporterImpl>,
}

#[repr(C)]
pub struct OtelPeriodicMetricReader {
    header: OtelHandleHeader,
    pub(crate) reader: PeriodicMetricReaderImpl,
}

impl HasHandleHeader for OtelPeriodicMetricReaderBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_PERIODIC_METRIC_READER_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

impl HasHandleHeader for OtelPeriodicMetricReader {
    const KIND: u64 = OTEL_HANDLE_KIND_PERIODIC_METRIC_READER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

#[no_mangle]
pub extern "C" fn otel_periodic_metric_reader_builder_new() -> *mut OtelPeriodicMetricReaderBuilder
{
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelPeriodicMetricReaderBuilder {
            header: OtelHandleHeader::new(OtelPeriodicMetricReaderBuilder::KIND),
            interval: None,
            timeout: None,
            runtime: READER_RUNTIME_BLOCKING,
            exporter: None,
        })
    })
}

/// Select the periodic reader runtime implementation.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_set_runtime(
    builder: *mut OtelPeriodicMetricReaderBuilder,
    runtime: u32,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if runtime > READER_RUNTIME_ASYNC {
            return fail(
                OtelStatus::InvalidArgument,
                "unknown periodic metric reader runtime",
            );
        }
        builder.runtime = runtime;
        OtelStatus::Ok
    })
}

/// Destroy a periodic reader builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_destroy(
    builder: *mut OtelPeriodicMetricReaderBuilder,
) {
    guard_unit(|| unsafe { destroy(builder) });
}

/// Set the periodic collection interval.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_set_interval_millis(
    builder: *mut OtelPeriodicMetricReaderBuilder,
    interval_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        builder.interval = (interval_millis != 0).then(|| Duration::from_millis(interval_millis));
        OtelStatus::Ok
    })
}

/// Set the async reader's per-export timeout.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_set_timeout_millis(
    builder: *mut OtelPeriodicMetricReaderBuilder,
    timeout_millis: u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        builder.timeout = (timeout_millis != 0).then(|| Duration::from_millis(timeout_millis));
        OtelStatus::Ok
    })
}

/// Transfer an exporter into a periodic reader builder. On success the original exporter
/// pointer is invalid; on failure the exporter remains caller-owned.
///
/// # Safety
///
/// `builder` and `exporter` must be live handles and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_set_exporter(
    builder: *mut OtelPeriodicMetricReaderBuilder,
    exporter: *mut OtelMetricExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.exporter.is_some() {
            return fail(
                OtelStatus::InvalidConfig,
                "periodic metric reader already has an exporter",
            );
        }
        let exporter = match unsafe { take(exporter) } {
            Some(exporter) => exporter,
            None => return OtelStatus::InvalidArgument,
        };
        builder.exporter = Some(exporter.exporter);
        OtelStatus::Ok
    })
}

#[cfg(feature = "metrics-async-runtime")]
#[derive(Clone, Debug)]
struct SdkAsyncRuntime(tokio::runtime::Handle);

#[cfg(feature = "metrics-async-runtime")]
impl Runtime for SdkAsyncRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        drop(self.0.spawn(future));
    }

    #[allow(clippy::manual_async_fn)]
    fn delay(&self, duration: Duration) -> impl std::future::Future<Output = ()> + Send + 'static {
        async move { tokio::time::sleep(duration).await }
    }
}

#[cfg(feature = "metrics-async-runtime")]
pub(crate) struct AsyncRuntimeGuard(Option<tokio::runtime::Runtime>);

#[cfg(feature = "metrics-async-runtime")]
impl AsyncRuntimeGuard {
    pub(crate) fn is_current(&self) -> bool {
        match (&self.0, tokio::runtime::Handle::try_current()) {
            (Some(runtime), Ok(current)) => current.id() == runtime.handle().id(),
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn handle(&self) -> tokio::runtime::Handle {
        self.0
            .as_ref()
            .expect("async Metrics runtime remains live")
            .handle()
            .clone()
    }
}

#[cfg(feature = "metrics-async-runtime")]
impl Drop for AsyncRuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(current) if current.id() == runtime.handle().id() => {
                    // Reentrant SDK destruction from its own collection callback is unsupported.
                    // Avoid a self-join panic if that contract is violated internally.
                    runtime.shutdown_background();
                }
                Ok(_) => {
                    std::thread::Builder::new()
                        .name("otel-c-metrics-async-shutdown".to_owned())
                        .spawn(move || drop(runtime))
                        .expect("spawn async Metrics runtime disposer")
                        .join()
                        .expect("async Metrics runtime disposer panicked");
                }
                Err(_) => drop(runtime),
            }
        }
    }
}

#[cfg(feature = "metrics-async-runtime")]
fn build_async_reader(
    exporter: MetricExporterImpl,
    interval: Option<Duration>,
    timeout: Option<Duration>,
) -> Result<PeriodicMetricReaderImpl, OtelStatus> {
    #[cfg(feature = "otlp-http")]
    if matches!(&exporter, MetricExporterImpl::OtlpHttp(_)) {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "the blocking OTLP/HTTP Metrics exporter is incompatible with the async reader",
        ));
    }
    #[cfg(feature = "otlp-grpc")]
    if matches!(&exporter, MetricExporterImpl::OtlpGrpc(_)) {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "the synchronous OTLP/gRPC Metrics exporter is incompatible with the async reader",
        ));
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_time()
        .thread_name("otel-c-metrics-async")
        .build()
        .map_err(|error| {
            fail_owned(
                OtelStatus::InternalError,
                format!("failed to build async Metrics runtime: {error}"),
            )
        })?;
    let async_runtime = SdkAsyncRuntime(runtime.handle().clone());
    let mut builder =
        opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader::builder(
            exporter,
            async_runtime,
        );
    if let Some(interval) = interval {
        builder = builder.with_interval(interval);
    }
    if let Some(timeout) = timeout {
        builder = builder.with_timeout(timeout);
    }
    Ok(PeriodicMetricReaderImpl::Async {
        reader: builder.build(),
        runtime: AsyncRuntimeGuard(Some(runtime)),
    })
}

fn build_reader(
    exporter: MetricExporterImpl,
    interval: Option<Duration>,
) -> PeriodicMetricReaderImpl {
    match exporter {
        #[cfg(test)]
        MetricExporterImpl::Test(exporter) => {
            let builder = PeriodicReader::builder(MetricExporterImpl::Test(exporter));
            let builder = match interval {
                Some(interval) => builder.with_interval(interval),
                None => builder,
            };
            PeriodicMetricReaderImpl::Test {
                reader: builder.build(),
                configured_interval: interval,
            }
        }
        exporter => {
            let builder: PeriodicReaderBuilder<_> = PeriodicReader::builder(exporter);
            let builder = match interval {
                Some(interval) => builder.with_interval(interval),
                None => builder,
            };
            PeriodicMetricReaderImpl::Reader(builder.build())
        }
    }
}

/// Build an owned periodic Metrics reader.
///
/// # Safety
///
/// `builder` must be live and `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_builder_build(
    builder: *mut OtelPeriodicMetricReaderBuilder,
    out: *mut *mut OtelPeriodicMetricReader,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if builder.runtime == READER_RUNTIME_BLOCKING && builder.timeout.is_some() {
            return fail(
                OtelStatus::InvalidConfig,
                "periodic metric reader export timeout requires the async runtime",
            );
        }
        if builder.runtime == READER_RUNTIME_ASYNC {
            #[cfg(not(feature = "metrics-async-runtime"))]
            return fail(
                OtelStatus::InvalidConfig,
                "async periodic Metrics reader is unavailable: rebuild with \
                 `metrics-async-runtime`",
            );

            #[cfg(all(feature = "metrics-async-runtime", feature = "otlp-grpc"))]
            if matches!(
                builder.exporter.as_ref(),
                Some(MetricExporterImpl::OtlpGrpc(_))
            ) {
                return fail(
                    OtelStatus::InvalidConfig,
                    "the synchronous OTLP/gRPC Metrics exporter is incompatible with the async \
                     reader",
                );
            }
            #[cfg(all(feature = "metrics-async-runtime", feature = "otlp-http"))]
            if matches!(
                builder.exporter.as_ref(),
                Some(MetricExporterImpl::OtlpHttp(_))
            ) {
                return fail(
                    OtelStatus::InvalidConfig,
                    "the blocking OTLP/HTTP Metrics exporter is incompatible with the async \
                      reader",
                );
            }
        }
        let exporter = match builder.exporter.take() {
            Some(exporter) => exporter,
            None => {
                return fail(
                    OtelStatus::InvalidConfig,
                    "periodic metric reader requires an exporter",
                )
            }
        };
        let reader = if builder.runtime == READER_RUNTIME_ASYNC {
            #[cfg(feature = "metrics-async-runtime")]
            {
                match build_async_reader(exporter, builder.interval, builder.timeout) {
                    Ok(reader) => reader,
                    Err(status) => return status,
                }
            }
            #[cfg(not(feature = "metrics-async-runtime"))]
            {
                unreachable!("async runtime availability was validated before exporter transfer")
            }
        } else {
            build_reader(exporter, builder.interval)
        };
        unsafe {
            *out = into_raw(OtelPeriodicMetricReader {
                header: OtelHandleHeader::new(OtelPeriodicMetricReader::KIND),
                reader,
            })
        };
        OtelStatus::Ok
    })
}

/// Destroy an untransferred periodic Metrics reader handle. After a successful transfer into an
/// SDK builder, the original pointer is invalid and must not be passed here.
///
/// # Safety
///
/// `reader` must be NULL or a live reader and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_destroy(
    reader: *mut OtelPeriodicMetricReader,
) {
    guard_unit(|| {
        #[cfg(feature = "metrics-async-runtime")]
        if unsafe { checked_ref(reader) }.is_some_and(|reader: &OtelPeriodicMetricReader| {
            reader.reader.is_current_async_runtime()
        }) {
            let _ = fail(
                OtelStatus::InvalidConfig,
                "cannot destroy an async periodic Metrics reader from its own runtime callback",
            );
            return;
        }
        if let Some(reader) = unsafe { take(reader) } {
            reader.reader.shutdown();
        }
    });
}

#[cfg(test)]
pub(crate) fn test_reader_with_lifecycle(
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    lifecycle: crate::metric_exporter::TestMetricExporterLifecycle,
) -> *mut OtelPeriodicMetricReader {
    let exporter = crate::metric_exporter::TestMetricExporter::with_lifecycle(drops, lifecycle);
    into_raw(OtelPeriodicMetricReader {
        header: OtelHandleHeader::new(OtelPeriodicMetricReader::KIND),
        reader: PeriodicMetricReaderImpl::Test {
            reader: PeriodicReader::builder(MetricExporterImpl::Test(exporter)).build(),
            configured_interval: None,
        },
    })
}

#[cfg(all(test, feature = "metrics-async-runtime"))]
pub(crate) fn test_async_reader(
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> *mut OtelPeriodicMetricReader {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_time()
        .build()
        .unwrap();
    let reader =
        opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader::builder(
            MetricExporterImpl::Test(crate::metric_exporter::TestMetricExporter::new(drops)),
            SdkAsyncRuntime(runtime.handle().clone()),
        )
        .with_interval(Duration::from_secs(60))
        .build();
    into_raw(OtelPeriodicMetricReader {
        header: OtelHandleHeader::new(OtelPeriodicMetricReader::KIND),
        reader: PeriodicMetricReaderImpl::Async {
            reader,
            runtime: AsyncRuntimeGuard(Some(runtime)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use crate::metric_exporter::otel_metric_exporter_destroy;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use crate::metric_exporter::TestMetricExporterLifecycle;
    use crate::metric_exporter::{OtelMetricExporter, TestMetricExporter};
    #[cfg(all(feature = "metrics-async-runtime", feature = "otlp-grpc"))]
    use crate::otlp_metric_exporter::otel_otlp_metric_exporter_builder_set_transport;
    #[cfg(all(
        feature = "metrics-async-runtime",
        any(feature = "otlp-http", feature = "otlp-grpc")
    ))]
    use crate::otlp_metric_exporter::{
        otel_otlp_metric_exporter_builder_build, otel_otlp_metric_exporter_builder_destroy,
        otel_otlp_metric_exporter_builder_new, otel_otlp_metric_exporter_builder_set_endpoint,
    };
    #[cfg(feature = "metrics-async-runtime")]
    use opentelemetry::metrics::MeterProvider;
    #[cfg(all(
        feature = "metrics-async-runtime",
        any(feature = "otlp-http", feature = "otlp-grpc")
    ))]
    use opentelemetry_c_abi::OtelStringView;
    #[cfg(feature = "metrics-async-runtime")]
    use opentelemetry_sdk::error::OTelSdkError;
    #[cfg(feature = "metrics-async-runtime")]
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use std::sync::{Condvar, Mutex};

    fn test_exporter(drops: &Arc<AtomicUsize>) -> *mut OtelMetricExporter {
        into_raw(OtelMetricExporter::new(MetricExporterImpl::Test(
            TestMetricExporter::new(Arc::clone(drops)),
        )))
    }

    #[cfg(feature = "metrics-async-runtime")]
    fn async_test_exporter(
        drops: &Arc<AtomicUsize>,
        exports: &Arc<AtomicUsize>,
        delay: Duration,
    ) -> *mut OtelMetricExporter {
        into_raw(OtelMetricExporter::new(MetricExporterImpl::Test(
            TestMetricExporter::with_async_probe(Arc::clone(drops), Arc::clone(exports), delay),
        )))
    }

    #[cfg(feature = "metrics-async-runtime")]
    unsafe fn build_async_test_reader(
        interval: Duration,
        delay: Duration,
        timeout: Duration,
        drops: &Arc<AtomicUsize>,
        exports: &Arc<AtomicUsize>,
    ) -> (AsyncPeriodicReader, AsyncRuntimeGuard) {
        let builder = otel_periodic_metric_reader_builder_new();
        assert_eq!(
            unsafe {
                otel_periodic_metric_reader_builder_set_runtime(builder, READER_RUNTIME_ASYNC)
            },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe {
                otel_periodic_metric_reader_builder_set_interval_millis(
                    builder,
                    u64::try_from(interval.as_millis()).unwrap(),
                )
            },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe {
                otel_periodic_metric_reader_builder_set_timeout_millis(
                    builder,
                    u64::try_from(timeout.as_millis()).unwrap(),
                )
            },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe {
                otel_periodic_metric_reader_builder_set_exporter(
                    builder,
                    async_test_exporter(drops, exports, delay),
                )
            },
            OtelStatus::Ok
        );
        let mut reader = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_periodic_metric_reader_builder_build(builder, &mut reader) },
            OtelStatus::Ok
        );
        unsafe { otel_periodic_metric_reader_builder_destroy(builder) };
        let reader = unsafe { take(reader) }.unwrap();
        match reader.reader {
            PeriodicMetricReaderImpl::Async { reader, runtime } => (reader, runtime),
            _ => panic!("expected async periodic reader"),
        }
    }

    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    fn lifecycle_exporter(
        drops: &Arc<AtomicUsize>,
        shutdowns: &Arc<AtomicUsize>,
        dropped: &Arc<(Mutex<bool>, Condvar)>,
    ) -> *mut OtelMetricExporter {
        into_raw(OtelMetricExporter::new(MetricExporterImpl::Test(
            TestMetricExporter::with_lifecycle(
                Arc::clone(drops),
                TestMetricExporterLifecycle {
                    shutdowns: Arc::clone(shutdowns),
                    dropped: Arc::clone(dropped),
                },
            ),
        )))
    }

    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    fn wait_for_drop(dropped: &Arc<(Mutex<bool>, Condvar)>) {
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
    fn interval_setter_validates_handles_and_uses_last_value() {
        unsafe {
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(std::ptr::null_mut(), 10,),
                OtelStatus::InvalidArgument
            );

            let dead = Box::into_raw(Box::new(OtelPeriodicMetricReaderBuilder {
                header: {
                    let mut header = OtelHandleHeader::new(OtelPeriodicMetricReaderBuilder::KIND);
                    header.poison();
                    header
                },
                interval: None,
                timeout: None,
                runtime: READER_RUNTIME_BLOCKING,
                exporter: None,
            }));
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(dead, 10),
                OtelStatus::InvalidArgument
            );
            drop(Box::from_raw(dead));

            let builder = otel_periodic_metric_reader_builder_new();
            assert!((*builder).interval.is_none());
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 100),
                OtelStatus::Ok
            );
            assert_eq!((*builder).interval, Some(Duration::from_millis(100)));
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 250),
                OtelStatus::Ok
            );
            assert_eq!((*builder).interval, Some(Duration::from_millis(250)));
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 0),
                OtelStatus::Ok
            );
            assert!(
                (*builder).interval.is_none(),
                "zero must leave interval selection to the upstream default/environment"
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 75),
                OtelStatus::Ok
            );
            assert_eq!((*builder).interval, Some(Duration::from_millis(75)));
            otel_periodic_metric_reader_builder_destroy(builder);
        }
    }

    #[test]
    fn runtime_and_timeout_setters_validate_and_preserve_blocking_default() {
        unsafe {
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(std::ptr::null_mut(), 0),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_timeout_millis(std::ptr::null_mut(), 10),
                OtelStatus::InvalidArgument
            );

            let builder = otel_periodic_metric_reader_builder_new();
            assert_eq!((*builder).runtime, READER_RUNTIME_BLOCKING);
            assert!((*builder).timeout.is_none());
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(builder, 2),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(builder, READER_RUNTIME_ASYNC),
                OtelStatus::Ok
            );
            assert_eq!((*builder).runtime, READER_RUNTIME_ASYNC);
            assert_eq!(
                otel_periodic_metric_reader_builder_set_timeout_millis(builder, 250),
                OtelStatus::Ok
            );
            assert_eq!((*builder).timeout, Some(Duration::from_millis(250)));
            assert_eq!(
                otel_periodic_metric_reader_builder_set_timeout_millis(builder, 0),
                OtelStatus::Ok
            );
            assert!((*builder).timeout.is_none());
            otel_periodic_metric_reader_builder_destroy(builder);
        }
    }

    #[test]
    fn blocking_reader_rejects_async_only_timeout_without_consuming_exporter() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_timeout_millis(builder, 10),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, test_exporter(&drops)),
                OtelStatus::Ok
            );
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            otel_periodic_metric_reader_builder_destroy(builder);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[cfg(not(feature = "metrics-async-runtime"))]
    #[test]
    fn unavailable_async_reader_names_required_feature() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(builder, READER_RUNTIME_ASYNC),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, test_exporter(&drops)),
                OtelStatus::Ok
            );
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert!(crate::api_ffi::test_probe::last_error().contains("metrics-async-runtime"));
            otel_periodic_metric_reader_builder_destroy(builder);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn async_reader_maps_timeout_for_cooperative_exporter_and_flushes_successfully() {
        let timeout_drops = Arc::new(AtomicUsize::new(0));
        let timeout_exports = Arc::new(AtomicUsize::new(0));
        let (reader, runtime) = unsafe {
            build_async_test_reader(
                Duration::from_secs(60),
                Duration::from_millis(100),
                Duration::from_millis(10),
                &timeout_drops,
                &timeout_exports,
            )
        };
        let control = reader.clone();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("async-timeout");
        meter.u64_counter("requests").build().add(1, &[]);
        assert!(matches!(
            control.force_flush(),
            Err(OTelSdkError::Timeout(timeout)) if timeout == Duration::from_millis(10)
        ));
        assert_eq!(timeout_exports.load(Ordering::SeqCst), 1);
        let _ = provider.shutdown();
        drop(meter);
        drop(provider);
        drop(control);
        drop(runtime);
        assert_eq!(timeout_drops.load(Ordering::SeqCst), 1);

        let success_drops = Arc::new(AtomicUsize::new(0));
        let success_exports = Arc::new(AtomicUsize::new(0));
        let (reader, runtime) = unsafe {
            build_async_test_reader(
                Duration::from_secs(60),
                Duration::from_millis(1),
                Duration::from_secs(1),
                &success_drops,
                &success_exports,
            )
        };
        let control = reader.clone();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("async-success");
        meter.u64_counter("requests").build().add(1, &[]);
        assert!(control.force_flush().is_ok());
        assert_eq!(success_exports.load(Ordering::SeqCst), 1);
        assert!(provider.shutdown().is_ok());
        drop(meter);
        drop(provider);
        drop(control);
        drop(runtime);
        assert_eq!(success_drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn multiple_async_readers_flush_independently() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let first_exports = Arc::new(AtomicUsize::new(0));
        let second_drops = Arc::new(AtomicUsize::new(0));
        let second_exports = Arc::new(AtomicUsize::new(0));
        let (first, first_runtime) = unsafe {
            build_async_test_reader(
                Duration::from_secs(60),
                Duration::ZERO,
                Duration::from_secs(1),
                &first_drops,
                &first_exports,
            )
        };
        let (second, second_runtime) = unsafe {
            build_async_test_reader(
                Duration::from_secs(60),
                Duration::ZERO,
                Duration::from_secs(1),
                &second_drops,
                &second_exports,
            )
        };
        let provider = SdkMeterProvider::builder()
            .with_reader(first)
            .with_reader(second)
            .build();
        let meter = provider.meter("async-multiple");
        meter.u64_counter("requests").build().add(1, &[]);
        assert!(provider.force_flush().is_ok());
        assert_eq!(first_exports.load(Ordering::SeqCst), 1);
        assert_eq!(second_exports.load(Ordering::SeqCst), 1);
        assert!(provider.shutdown().is_ok());
        drop(meter);
        drop(provider);
        drop(first_runtime);
        drop(second_runtime);
        assert_eq!(first_drops.load(Ordering::SeqCst), 1);
        assert_eq!(second_drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn async_reader_collects_on_configured_interval() {
        let drops = Arc::new(AtomicUsize::new(0));
        let exports = Arc::new(AtomicUsize::new(0));
        let (reader, runtime) = unsafe {
            build_async_test_reader(
                Duration::from_millis(10),
                Duration::ZERO,
                Duration::from_secs(1),
                &drops,
                &exports,
            )
        };
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("async-interval");
        meter.u64_counter("requests").build().add(1, &[]);
        let exports_before_wait = exports.load(Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while exports.load(Ordering::SeqCst) <= exports_before_wait {
            assert!(
                std::time::Instant::now() < deadline,
                "async reader did not export within the configured interval"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(provider.shutdown().is_ok());
        drop(meter);
        drop(provider);
        drop(runtime);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(all(feature = "metrics-async-runtime", feature = "otlp-grpc"))]
    #[test]
    fn async_reader_rejects_synchronous_grpc_exporter() {
        unsafe {
            let exporter_builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(exporter_builder, 1),
                OtelStatus::Ok
            );
            let endpoint = "http://127.0.0.1:9";
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(
                    exporter_builder,
                    OtelStringView {
                        ptr: endpoint.as_ptr().cast(),
                        len: endpoint.len(),
                    },
                ),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(exporter_builder, &mut exporter),
                OtelStatus::Ok
            );
            otel_otlp_metric_exporter_builder_destroy(exporter_builder);

            let reader_builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(
                    reader_builder,
                    READER_RUNTIME_ASYNC,
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(reader_builder, exporter),
                OtelStatus::Ok
            );
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(reader_builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            assert!(crate::api_ffi::test_probe::last_error()
                .contains("synchronous OTLP/gRPC Metrics exporter"));

            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(
                    reader_builder,
                    READER_RUNTIME_BLOCKING,
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_build(reader_builder, &mut reader),
                OtelStatus::Ok
            );
            otel_periodic_metric_reader_destroy(reader);
            otel_periodic_metric_reader_builder_destroy(reader_builder);
        }
    }

    #[cfg(all(feature = "metrics-async-runtime", feature = "otlp-http"))]
    #[test]
    fn async_reader_rejects_blocking_http_exporter() {
        unsafe {
            let exporter_builder = otel_otlp_metric_exporter_builder_new();
            let endpoint = "http://127.0.0.1:9/v1/metrics";
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(
                    exporter_builder,
                    OtelStringView {
                        ptr: endpoint.as_ptr().cast(),
                        len: endpoint.len(),
                    },
                ),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(exporter_builder, &mut exporter),
                OtelStatus::Ok
            );
            otel_otlp_metric_exporter_builder_destroy(exporter_builder);

            let reader_builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(
                    reader_builder,
                    READER_RUNTIME_ASYNC,
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(reader_builder, exporter),
                OtelStatus::Ok
            );
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(reader_builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            assert!(crate::api_ffi::test_probe::last_error()
                .contains("blocking OTLP/HTTP Metrics exporter"));

            assert_eq!(
                otel_periodic_metric_reader_builder_set_runtime(
                    reader_builder,
                    READER_RUNTIME_BLOCKING,
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_build(reader_builder, &mut reader),
                OtelStatus::Ok
            );
            otel_periodic_metric_reader_destroy(reader);
            otel_periodic_metric_reader_builder_destroy(reader_builder);
        }
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn async_runtime_disposal_is_synchronous_inside_another_tokio_runtime() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_time();
        let stopped_probe = Arc::clone(&stopped);
        builder.on_thread_stop(move || {
            stopped_probe.fetch_add(1, Ordering::SeqCst);
        });
        let guard = AsyncRuntimeGuard(Some(builder.build().unwrap()));
        let host = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        host.block_on(async move { drop(guard) });
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            1,
            "async Metrics runtime worker must stop before destruction returns"
        );
    }

    #[cfg(feature = "metrics-async-runtime")]
    #[test]
    fn async_reader_destroy_fails_closed_on_its_own_runtime() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let reader = test_async_reader(Arc::clone(&drops));
            let runtime = match &(*reader).reader {
                PeriodicMetricReaderImpl::Async { runtime, .. } => runtime.handle(),
                _ => panic!("expected async reader"),
            };
            runtime.block_on(async {
                otel_periodic_metric_reader_destroy(reader);
                assert!(crate::api_ffi::test_probe::last_error()
                    .contains("from its own runtime callback"));
            });
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            drop(runtime);
            otel_periodic_metric_reader_destroy(reader);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    #[test]
    fn exporter_transfer_duplicate_rejection_and_reader_destruction_are_exactly_once() {
        unsafe {
            let first_drops = Arc::new(AtomicUsize::new(0));
            let first_shutdowns = Arc::new(AtomicUsize::new(0));
            let first_dropped = Arc::new((Mutex::new(false), Condvar::new()));
            let second_drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 100),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 250),
                OtelStatus::Ok
            );
            let first = lifecycle_exporter(&first_drops, &first_shutdowns, &first_dropped);
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, first),
                OtelStatus::Ok
            );
            assert_eq!(first_drops.load(Ordering::SeqCst), 0);

            let second = test_exporter(&second_drops);
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, second),
                OtelStatus::InvalidConfig
            );
            assert_eq!(second_drops.load(Ordering::SeqCst), 0);
            otel_metric_exporter_destroy(second);
            assert_eq!(second_drops.load(Ordering::SeqCst), 1);

            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(builder, &mut reader),
                OtelStatus::Ok
            );
            assert!(!reader.is_null());
            match &(*reader).reader {
                PeriodicMetricReaderImpl::Test {
                    configured_interval,
                    ..
                } => assert_eq!(*configured_interval, Some(Duration::from_millis(250))),
                PeriodicMetricReaderImpl::Reader(_) => panic!("expected test reader"),
                #[cfg(feature = "metrics-async-runtime")]
                PeriodicMetricReaderImpl::Async { .. } => panic!("expected test reader"),
            }
            otel_periodic_metric_reader_builder_destroy(builder);
            assert_eq!(first_drops.load(Ordering::SeqCst), 0);
            otel_periodic_metric_reader_destroy(reader);
            assert_eq!(first_shutdowns.load(Ordering::SeqCst), 1);
            wait_for_drop(&first_dropped);
            assert_eq!(first_drops.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn reader_build_failure_preserves_or_releases_owned_exporter_correctly() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
            assert_eq!(
                otel_periodic_metric_reader_builder_set_interval_millis(builder, 25),
                OtelStatus::Ok
            );
            let exporter = test_exporter(&drops);
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, exporter),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_periodic_metric_reader_builder_build(builder, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            otel_periodic_metric_reader_builder_destroy(builder);
            assert_eq!(drops.load(Ordering::SeqCst), 1);

            let empty_builder = otel_periodic_metric_reader_builder_new();
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(empty_builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            otel_periodic_metric_reader_builder_destroy(empty_builder);
        }
    }
}
