//! Opaque Metrics exporter handle.

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use std::time::Duration;

#[cfg(feature = "otlp-grpc")]
use opentelemetry_sdk::error::OTelSdkError;
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use opentelemetry_sdk::error::OTelSdkResult;
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use opentelemetry_sdk::metrics::data::ResourceMetrics;
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
use opentelemetry_sdk::metrics::Temporality;

use crate::handle::{destroy, guard_unit, HasMagic};

pub(crate) enum MetricExporterImpl {
    #[cfg(feature = "otlp-http")]
    OtlpHttp(opentelemetry_otlp::MetricExporter),
    #[cfg(feature = "otlp-grpc")]
    OtlpGrpc(GrpcMetricExporter),
    #[cfg(test)]
    #[allow(dead_code)]
    Test(TestMetricExporter),
}

#[cfg(feature = "otlp-grpc")]
pub(crate) struct GrpcMetricExporter {
    // Drop explicitly takes the exporter before releasing the runtime guard. Options prevent
    // automatic field drop from creating a second, ordering-independent destruction path.
    exporter: Option<opentelemetry_otlp::MetricExporter>,
    runtime: Option<GrpcRuntimeGuard>,
}

#[cfg(feature = "otlp-grpc")]
pub(crate) struct GrpcRuntimeGuard(Option<tokio::runtime::Runtime>);

#[cfg(feature = "otlp-grpc")]
impl GrpcRuntimeGuard {
    pub(crate) fn new(runtime: tokio::runtime::Runtime) -> Self {
        Self(Some(runtime))
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Runtime {
        self.0
            .as_ref()
            .expect("gRPC runtime is present until guard drop")
    }
}

#[cfg(feature = "otlp-grpc")]
impl Drop for GrpcRuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            dispose_grpc_runtime(runtime);
        }
    }
}

#[cfg(feature = "otlp-grpc")]
impl GrpcMetricExporter {
    pub(crate) fn new(
        exporter: opentelemetry_otlp::MetricExporter,
        runtime: GrpcRuntimeGuard,
    ) -> Self {
        Self {
            exporter: Some(exporter),
            runtime: Some(runtime),
        }
    }

    fn exporter(&self) -> &opentelemetry_otlp::MetricExporter {
        self.exporter
            .as_ref()
            .expect("gRPC exporter is present until drop")
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("gRPC runtime is present until drop")
            .runtime()
    }
}

#[cfg(feature = "otlp-grpc")]
impl Drop for GrpcMetricExporter {
    fn drop(&mut self) {
        let runtime = self.runtime.take();
        drop(self.exporter.take());
        drop(runtime);
    }
}

#[cfg(feature = "otlp-grpc")]
fn dispose_grpc_runtime(runtime: tokio::runtime::Runtime) {
    if tokio::runtime::Handle::try_current().is_ok() {
        runtime.shutdown_background();
    } else {
        drop(runtime);
    }
}

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
impl PushMetricExporter for MetricExporterImpl {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.export(metrics).await,
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => {
                // The current thread-based PeriodicReader invokes export from its dedicated
                // OS thread. Fail closed if a future reader drives this synchronous wrapper
                // from inside Tokio, where Runtime::block_on would panic.
                if tokio::runtime::Handle::try_current().is_ok() {
                    return Err(OTelSdkError::InternalFailure(
                        "synchronous OTLP gRPC Metrics export cannot call block_on from an \
                         entered Tokio runtime"
                            .to_owned(),
                    ));
                }
                exporter
                    .runtime()
                    .block_on(exporter.exporter().export(metrics))
            }
            #[cfg(test)]
            Self::Test(exporter) => exporter.export(metrics).await,
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.force_flush(),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => exporter.exporter().force_flush(),
            #[cfg(test)]
            Self::Test(exporter) => exporter.force_flush(),
        }
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.shutdown_with_timeout(timeout),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => exporter.exporter().shutdown_with_timeout(timeout),
            #[cfg(test)]
            Self::Test(exporter) => exporter.shutdown_with_timeout(timeout),
        }
    }

    fn temporality(&self) -> Temporality {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.temporality(),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => exporter.exporter().temporality(),
            #[cfg(test)]
            Self::Test(exporter) => exporter.temporality(),
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestMetricExporter {
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    lifecycle: Option<TestMetricExporterLifecycle>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct TestMetricExporterLifecycle {
    pub(crate) shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) dropped: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(test)]
impl TestMetricExporter {
    pub(crate) fn new(drops: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            drops,
            lifecycle: None,
        }
    }

    pub(crate) fn with_lifecycle(
        drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        lifecycle: TestMetricExporterLifecycle,
    ) -> Self {
        Self {
            drops,
            lifecycle: Some(lifecycle),
        }
    }
}

#[cfg(test)]
impl Drop for TestMetricExporter {
    fn drop(&mut self) {
        self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(lifecycle) = &self.lifecycle {
            let (dropped, condition) = &*lifecycle.dropped;
            *dropped
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            condition.notify_all();
        }
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
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle
                .shutdowns
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
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
