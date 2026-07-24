//! Periodic Metrics reader builder and transferable reader handle.

use std::time::Duration;

use opentelemetry_c_abi::OtelStatus;

use crate::error::{clear_last_error, fail};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, take, HasMagic,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

#[cfg(feature = "otlp")]
use opentelemetry_sdk::metrics::{PeriodicReader, PeriodicReaderBuilder};

const BUILDER_MAGIC: u64 = 0x4F54_4C43_4D52_4442;
const READER_MAGIC: u64 = 0x4F54_4C43_4D52_4452;

#[cfg(feature = "otlp")]
pub(crate) enum PeriodicMetricReaderImpl {
    Otlp(PeriodicReader<opentelemetry_otlp::MetricExporter>),
}

#[cfg(not(feature = "otlp"))]
pub(crate) enum PeriodicMetricReaderImpl {}

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
    guard_unit(|| unsafe { destroy(reader) });
}
