//! Opaque Metrics exporter handle.

use crate::handle::{destroy, guard_unit, HasMagic};

pub(crate) enum MetricExporterImpl {
    #[cfg(feature = "otlp")]
    Otlp(opentelemetry_otlp::MetricExporter),
    #[cfg(test)]
    #[allow(dead_code)]
    Test(TestMetricExporter),
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestMetricExporter {
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl TestMetricExporter {
    pub(crate) fn new(drops: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self { drops }
    }
}

#[cfg(test)]
impl Drop for TestMetricExporter {
    fn drop(&mut self) {
        self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl opentelemetry_sdk::metrics::exporter::PushMetricExporter for TestMetricExporter {
    async fn export(
        &self,
        _metrics: &opentelemetry_sdk::metrics::data::ResourceMetrics,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(
        &self,
        _timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn temporality(&self) -> opentelemetry_sdk::metrics::Temporality {
        opentelemetry_sdk::metrics::Temporality::Cumulative
    }
}

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
