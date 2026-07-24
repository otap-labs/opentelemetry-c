//! Opaque Metrics exporter handle.

use crate::handle::{destroy, guard_unit, HasMagic};

#[cfg(feature = "otlp")]
pub(crate) enum MetricExporterImpl {
    Otlp(opentelemetry_otlp::MetricExporter),
}

#[cfg(not(feature = "otlp"))]
pub(crate) enum MetricExporterImpl {}

const METRIC_EXPORTER_MAGIC: u64 = 0x4F54_4C43_4D45_5850;

pub struct OtelMetricExporter {
    magic: u64,
    pub(crate) exporter: MetricExporterImpl,
}

impl OtelMetricExporter {
    pub(crate) fn new(exporter: MetricExporterImpl) -> Self {
        Self {
            magic: METRIC_EXPORTER_MAGIC,
            exporter,
        }
    }
}

impl HasMagic for OtelMetricExporter {
    const MAGIC: u64 = METRIC_EXPORTER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

/// Destroy a Metrics exporter handle.
///
/// # Safety
///
/// `exporter` must be NULL or a live exporter handle and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_exporter_destroy(exporter: *mut OtelMetricExporter) {
    guard_unit(|| unsafe { destroy(exporter) });
}
