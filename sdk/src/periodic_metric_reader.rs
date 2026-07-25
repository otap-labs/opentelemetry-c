//! Periodic Metrics reader builder and transferable reader handle.

use std::time::Duration;

use opentelemetry_c_abi::OtelStatus;

use crate::error::{clear_last_error, fail};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, take, HasMagic,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

#[cfg(feature = "otlp")]
use opentelemetry_sdk::metrics::PeriodicReaderBuilder;
#[cfg(any(feature = "otlp", test))]
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

const BUILDER_MAGIC: u64 = 0x4F54_4C43_4D52_4442;
const READER_MAGIC: u64 = 0x4F54_4C43_4D52_4452;

pub(crate) enum PeriodicMetricReaderImpl {
    #[cfg(feature = "otlp")]
    Otlp(PeriodicReader<opentelemetry_otlp::MetricExporter>),
    #[cfg(test)]
    Test(PeriodicReader<crate::metric_exporter::TestMetricExporter>),
}

impl PeriodicMetricReaderImpl {
    pub(crate) fn shutdown(self) {
        match self {
            #[cfg(feature = "otlp")]
            Self::Otlp(reader) => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
            }
            #[cfg(test)]
            Self::Test(reader) => {
                let provider = SdkMeterProvider::builder().with_reader(reader).build();
                let _ = provider.shutdown();
            }
        }
    }
}

pub struct OtelPeriodicMetricReaderBuilder {
    magic: u64,
    interval: Option<Duration>,
    exporter: Option<MetricExporterImpl>,
}

pub struct OtelPeriodicMetricReader {
    magic: u64,
    pub(crate) reader: PeriodicMetricReaderImpl,
}

impl HasMagic for OtelPeriodicMetricReaderBuilder {
    const MAGIC: u64 = BUILDER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

impl HasMagic for OtelPeriodicMetricReader {
    const MAGIC: u64 = READER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

#[no_mangle]
pub extern "C" fn otel_periodic_metric_reader_builder_new() -> *mut OtelPeriodicMetricReaderBuilder
{
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelPeriodicMetricReaderBuilder {
            magic: BUILDER_MAGIC,
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

#[cfg(feature = "otlp")]
fn build_reader(
    exporter: MetricExporterImpl,
    interval: Option<Duration>,
) -> PeriodicMetricReaderImpl {
    match exporter {
        MetricExporterImpl::Otlp(exporter) => {
            let builder: PeriodicReaderBuilder<_> = PeriodicReader::builder(exporter);
            let builder = match interval {
                Some(interval) => builder.with_interval(interval),
                None => builder,
            };
            PeriodicMetricReaderImpl::Otlp(builder.build())
        }
        #[cfg(test)]
        MetricExporterImpl::Test(exporter) => {
            let builder = PeriodicReader::builder(exporter);
            let builder = match interval {
                Some(interval) => builder.with_interval(interval),
                None => builder,
            };
            PeriodicMetricReaderImpl::Test(builder.build())
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
        #[cfg(feature = "otlp")]
        {
            let reader = build_reader(exporter, builder.interval);
            unsafe {
                *out = into_raw(OtelPeriodicMetricReader {
                    magic: READER_MAGIC,
                    reader,
                })
            };
            OtelStatus::Ok
        }
        #[cfg(not(feature = "otlp"))]
        {
            let _ = exporter;
            fail(
                OtelStatus::InvalidConfig,
                "periodic metric reader is unavailable without the `otlp` feature",
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
pub(crate) fn test_reader(
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> *mut OtelPeriodicMetricReader {
    let exporter = crate::metric_exporter::TestMetricExporter::new(drops);
    into_raw(OtelPeriodicMetricReader {
        magic: READER_MAGIC,
        reader: PeriodicMetricReaderImpl::Test(PeriodicReader::builder(exporter).build()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "otlp")]
    use crate::metric_exporter::otel_metric_exporter_destroy;
    #[cfg(feature = "otlp")]
    use crate::metric_exporter::TestMetricExporterLifecycle;
    use crate::metric_exporter::{OtelMetricExporter, TestMetricExporter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[cfg(feature = "otlp")]
    use std::sync::{Condvar, Mutex};

    fn test_exporter(drops: &Arc<AtomicUsize>) -> *mut OtelMetricExporter {
        into_raw(OtelMetricExporter::new(MetricExporterImpl::Test(
            TestMetricExporter::new(Arc::clone(drops)),
        )))
    }

    #[cfg(feature = "otlp")]
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

    #[cfg(feature = "otlp")]
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

    #[cfg(feature = "otlp")]
    #[test]
    fn exporter_transfer_duplicate_rejection_and_reader_destruction_are_exactly_once() {
        unsafe {
            let first_drops = Arc::new(AtomicUsize::new(0));
            let first_shutdowns = Arc::new(AtomicUsize::new(0));
            let first_dropped = Arc::new((Mutex::new(false), Condvar::new()));
            let second_drops = Arc::new(AtomicUsize::new(0));
            let builder = otel_periodic_metric_reader_builder_new();
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

    #[cfg(not(feature = "otlp"))]
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
