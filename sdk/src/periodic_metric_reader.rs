//! Periodic Metrics reader builder and transferable reader handle.

use std::time::Duration;

use opentelemetry_c_abi::{
    OtelHandleHeader, OtelStatus, OTEL_HANDLE_KIND_PERIODIC_METRIC_READER,
    OTEL_HANDLE_KIND_PERIODIC_METRIC_READER_BUILDER,
};

use crate::error::{clear_last_error, fail};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, take, HasHandleHeader,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
use opentelemetry_sdk::metrics::PeriodicReaderBuilder;
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

pub(crate) enum PeriodicMetricReaderImpl {
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    Otlp(PeriodicReader<MetricExporterImpl>),
    #[cfg(test)]
    Test {
        reader: PeriodicReader<MetricExporterImpl>,
        #[allow(dead_code)]
        configured_interval: Option<Duration>,
    },
}

impl PeriodicMetricReaderImpl {
    pub(crate) fn shutdown(self) {
        match self {
            #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
            Self::Otlp(reader) => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
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
            exporter: None,
        })
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

/// Transfer an exporter into a periodic reader builder.
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

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
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
            PeriodicMetricReaderImpl::Otlp(builder.build())
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
        let exporter = match builder.exporter.take() {
            Some(exporter) => exporter,
            None => {
                return fail(
                    OtelStatus::InvalidConfig,
                    "periodic metric reader requires an exporter",
                )
            }
        };
        #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
        {
            let reader = build_reader(exporter, builder.interval);
            unsafe {
                *out = into_raw(OtelPeriodicMetricReader {
                    header: OtelHandleHeader::new(OtelPeriodicMetricReader::KIND),
                    reader,
                })
            };
            OtelStatus::Ok
        }
        #[cfg(not(any(feature = "otlp-http", feature = "otlp-grpc")))]
        {
            let _ = exporter;
            fail(
                OtelStatus::InvalidConfig,
                "periodic metric reader is unavailable without an OTLP transport feature",
            )
        }
    })
}

/// Destroy a periodic Metrics reader handle.
///
/// # Safety
///
/// `reader` must be NULL or a live reader and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_periodic_metric_reader_destroy(
    reader: *mut OtelPeriodicMetricReader,
) {
    guard_unit(|| {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use crate::metric_exporter::otel_metric_exporter_destroy;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use crate::metric_exporter::TestMetricExporterLifecycle;
    use crate::metric_exporter::{OtelMetricExporter, TestMetricExporter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use std::sync::{Condvar, Mutex};

    fn test_exporter(drops: &Arc<AtomicUsize>) -> *mut OtelMetricExporter {
        into_raw(OtelMetricExporter::new(MetricExporterImpl::Test(
            TestMetricExporter::new(Arc::clone(drops)),
        )))
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
            otel_metric_exporter_destroy(first);
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
                #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
                PeriodicMetricReaderImpl::Otlp(_) => panic!("expected test reader"),
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

    #[cfg(not(any(feature = "otlp-http", feature = "otlp-grpc")))]
    #[test]
    fn unavailable_reader_build_releases_transferred_exporter_once() {
        unsafe {
            let drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
            let exporter = test_exporter(&drops);
            assert_eq!(
                otel_periodic_metric_reader_builder_set_exporter(builder, exporter),
                OtelStatus::Ok
            );
            let mut reader = std::ptr::null_mut();
            assert_eq!(
                otel_periodic_metric_reader_builder_build(builder, &mut reader),
                OtelStatus::InvalidConfig
            );
            assert!(reader.is_null());
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            otel_periodic_metric_reader_builder_destroy(builder);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }
}
