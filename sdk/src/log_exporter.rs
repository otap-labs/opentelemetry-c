//! Opaque Logs exporter handle.
//!
//! Mirrors [`crate::metric_exporter`]: an internal enum of concrete exporter kinds behind one
//! opaque C handle, so adding a transport never changes the public C ABI.

use std::time::Duration;

use opentelemetry::logs::Severity;
use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_LOG_EXPORTER};
#[cfg(feature = "otlp-grpc")]
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use opentelemetry_sdk::Resource;

use crate::handle::{destroy, guard_unit, HasHandleHeader};

#[derive(Debug)]
pub(crate) enum LogExporterImpl {
    #[cfg(feature = "otlp-http")]
    OtlpHttp(opentelemetry_otlp::LogExporter),
    #[cfg(feature = "otlp-grpc")]
    OtlpGrpc(GrpcLogExporter),
    #[cfg(test)]
    InMemory(opentelemetry_sdk::logs::InMemoryLogExporter),
}

/// An OTLP/gRPC log exporter bound to the SDK-owned Tokio runtime that drives it.
///
/// Both fields are `Option` so `Drop` can release the exporter *before* the runtime, which is
/// the only safe order (the exporter's transport tasks live on that runtime).
#[cfg(feature = "otlp-grpc")]
pub(crate) struct GrpcLogExporter {
    exporter: Option<opentelemetry_otlp::LogExporter>,
    runtime: Option<crate::metric_exporter::GrpcRuntimeGuard>,
}

#[cfg(feature = "otlp-grpc")]
impl std::fmt::Debug for GrpcLogExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcLogExporter").finish_non_exhaustive()
    }
}

#[cfg(feature = "otlp-grpc")]
impl GrpcLogExporter {
    pub(crate) fn new(
        exporter: opentelemetry_otlp::LogExporter,
        runtime: crate::metric_exporter::GrpcRuntimeGuard,
    ) -> Self {
        Self {
            exporter: Some(exporter),
            runtime: Some(runtime),
        }
    }

    fn exporter(&self) -> &opentelemetry_otlp::LogExporter {
        self.exporter
            .as_ref()
            .expect("gRPC log exporter is present until drop")
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("gRPC runtime is present until drop")
            .runtime()
    }
}

#[cfg(feature = "otlp-grpc")]
impl Drop for GrpcLogExporter {
    fn drop(&mut self) {
        let runtime = self.runtime.take();
        drop(self.exporter.take());
        drop(runtime);
    }
}

// Mirrors `TraceExporterImpl`: with every exporter feature disabled the enum is uninhabited,
// so the trait is implemented in a second, arm-free block instead of adding an unreachable
// placeholder variant.
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
impl LogExporter for LogExporterImpl {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.export(batch).await,
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => {
                // Both the simple and batch log processors drive this from a plain OS thread.
                // Fail closed rather than panic if some future processor calls it from inside
                // Tokio, where `Runtime::block_on` is forbidden.
                if tokio::runtime::Handle::try_current().is_ok() {
                    return Err(OTelSdkError::InternalFailure(
                        "synchronous OTLP gRPC Logs export cannot call block_on from an \
                         entered Tokio runtime"
                            .to_owned(),
                    ));
                }
                exporter
                    .runtime()
                    .block_on(exporter.exporter().export(batch))
            }
            #[cfg(test)]
            Self::InMemory(exporter) => exporter.export(batch).await,
        }
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.shutdown_with_timeout(timeout),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => exporter.exporter().shutdown_with_timeout(timeout),
            #[cfg(test)]
            Self::InMemory(exporter) => exporter.shutdown_with_timeout(timeout),
        }
    }

    fn event_enabled(&self, level: Severity, target: &str, name: Option<&str>) -> bool {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.event_enabled(level, target, name),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => exporter.exporter().event_enabled(level, target, name),
            #[cfg(test)]
            Self::InMemory(exporter) => exporter.event_enabled(level, target, name),
        }
    }

    fn set_resource(&mut self, resource: &Resource) {
        match self {
            #[cfg(feature = "otlp-http")]
            Self::OtlpHttp(exporter) => exporter.set_resource(resource),
            #[cfg(feature = "otlp-grpc")]
            Self::OtlpGrpc(exporter) => {
                if let Some(exporter) = exporter.exporter.as_mut() {
                    exporter.set_resource(resource);
                }
            }
            #[cfg(test)]
            Self::InMemory(exporter) => exporter.set_resource(resource),
        }
    }
}

#[cfg(not(any(feature = "otlp-http", feature = "otlp-grpc", test)))]
impl LogExporter for LogExporterImpl {
    async fn export(&self, _batch: LogBatch<'_>) -> OTelSdkResult {
        // Uninhabited when no exporter feature is enabled: cannot be constructed or called.
        match *self {}
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        match *self {}
    }
    fn event_enabled(&self, _level: Severity, _target: &str, _name: Option<&str>) -> bool {
        match *self {}
    }
    fn set_resource(&mut self, _resource: &Resource) {
        match *self {}
    }
}

/// Opaque log-exporter handle (`otel_log_exporter_t`). Owns a built exporter until it is
/// transferred into a log-processor constructor or destroyed.
#[repr(C)]
pub struct OtelLogExporter {
    header: OtelHandleHeader,
    pub(crate) exporter: LogExporterImpl,
}

impl OtelLogExporter {
    pub(crate) fn new(exporter: LogExporterImpl) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            exporter,
        }
    }
}

impl HasHandleHeader for OtelLogExporter {
    const KIND: u64 = OTEL_HANDLE_KIND_LOG_EXPORTER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Destroy a log-exporter handle (no-op on NULL).
///
/// Do **not** call this on an exporter that was successfully transferred into a log processor:
/// the original pointer is invalid after transfer.
///
/// # Safety
///
/// `exporter` must be NULL or a live log-exporter handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_log_exporter_destroy(exporter: *mut OtelLogExporter) {
    guard_unit(|| unsafe { destroy(exporter) });
}
