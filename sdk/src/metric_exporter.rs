// SPDX-License-Identifier: Apache-2.0

//! Opaque Metrics exporter handle.

use std::time::Duration;

use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_METRIC_EXPORTER};
#[cfg(feature = "otlp-grpc")]
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::Temporality;

use crate::custom_metric_exporter::{
    custom_exporter_export, custom_exporter_force_flush, custom_exporter_shutdown,
    CustomMetricExporter,
};
use crate::handle::{destroy, guard_unit, HasHandleHeader};

pub(crate) enum MetricExporterImpl {
    #[cfg(feature = "otlp-http")]
    OtlpHttp(opentelemetry_otlp::MetricExporter),
    #[cfg(feature = "otlp-grpc")]
    OtlpGrpc(GrpcMetricExporter),
    Custom(CustomMetricExporter),
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
    match tokio::runtime::Handle::try_current() {
        Ok(current) if current.id() == runtime.handle().id() => {
            // The runtime handle is private and the exporter is never moved into its tasks,
            // so public C destruction cannot reach this branch. Joining a disposer thread
            // from the runtime's own worker would deadlock; retain a non-panicking fail-safe
            // for an internal ownership violation.
            runtime.shutdown_background();
        }
        Ok(_) => {
            // Tokio forbids blocking Runtime destruction inside an entered runtime. Move
            // disposal to a neutral thread and join it so no SDK runtime work survives the
            // C destruction call. Dynamic-library unloading after use remains unsupported.
            std::thread::Builder::new()
                .name("otel-c-otlp-grpc-shutdown".to_owned())
                .spawn(move || drop(runtime))
                .expect("spawn OTLP gRPC runtime disposer")
                .join()
                .expect("OTLP gRPC runtime disposer panicked");
        }
        Err(_) => drop(runtime),
    }
}

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
            Self::Custom(exporter) => custom_exporter_export(exporter, metrics),
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
            Self::Custom(exporter) => custom_exporter_force_flush(exporter),
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
            Self::Custom(exporter) => custom_exporter_shutdown(exporter, timeout),
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
            Self::Custom(exporter) => exporter.temporality(),
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
    #[cfg(feature = "metrics-async-runtime")]
    exports: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(feature = "metrics-async-runtime")]
    export_delay: Option<Duration>,
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
            #[cfg(feature = "metrics-async-runtime")]
            exports: None,
            #[cfg(feature = "metrics-async-runtime")]
            export_delay: None,
        }
    }

    pub(crate) fn with_lifecycle(
        drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        lifecycle: TestMetricExporterLifecycle,
    ) -> Self {
        Self {
            drops,
            lifecycle: Some(lifecycle),
            #[cfg(feature = "metrics-async-runtime")]
            exports: None,
            #[cfg(feature = "metrics-async-runtime")]
            export_delay: None,
        }
    }

    #[cfg(feature = "metrics-async-runtime")]
    pub(crate) fn with_async_probe(
        drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        exports: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        export_delay: Duration,
    ) -> Self {
        Self {
            drops,
            lifecycle: None,
            exports: Some(exports),
            export_delay: Some(export_delay),
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
        #[cfg(feature = "metrics-async-runtime")]
        {
            if let Some(exports) = &self.exports {
                exports.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let Some(delay) = self.export_delay {
                tokio::time::sleep(delay).await;
            }
        }
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

#[repr(C)]
pub struct OtelMetricExporter {
    header: OtelHandleHeader,
    pub(crate) exporter: MetricExporterImpl,
}

impl OtelMetricExporter {
    pub(crate) fn new(exporter: MetricExporterImpl) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            exporter,
        }
    }
}

impl HasHandleHeader for OtelMetricExporter {
    const KIND: u64 = OTEL_HANDLE_KIND_METRIC_EXPORTER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Destroy an untransferred Metrics exporter handle. After a successful transfer into a reader,
/// the original pointer is invalid and must not be passed here.
///
/// # Safety
///
/// `exporter` must be NULL or a live exporter handle and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_exporter_destroy(exporter: *mut OtelMetricExporter) {
    guard_unit(|| unsafe { destroy(exporter) });
}
