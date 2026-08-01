//! The generic trace-exporter handle (`otel_trace_exporter_t`) and its internal
//! implementation enum.
//!
//! The opaque C handle wraps a `TraceExporterImpl` — an internal enum whose variants are the
//! concrete exporter kinds this SDK supports. It implements
//! [`opentelemetry_sdk::trace::SpanExporter`], so a span processor can drive it uniformly
//! regardless of which exporter is inside. The callback-backed custom exporter is SDK core;
//! the OTLP exporters are **optional** variants (features `otlp-http` and `otlp-grpc`).

use std::time::Duration;

use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_TRACE_EXPORTER};
#[cfg(feature = "otlp-grpc")]
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::Resource;

use crate::handle::{destroy, guard_unit, HasHandleHeader};

/// Internal trace-exporter implementation. Each variant is a concrete exporter kind; the enum
/// dispatches the [`SpanExporter`] trait to the active one. OTLP is optional.
#[derive(Debug)]
pub(crate) enum TraceExporterImpl {
    /// Callback-backed custom C exporter.
    Custom(crate::custom_trace_exporter::CustomTraceExporter),
    /// OTLP HTTP/protobuf exporter (optional; feature `otlp-http`).
    #[cfg(feature = "otlp-http")]
    OtlpHttp(opentelemetry_otlp::SpanExporter),
    /// OTLP gRPC exporter (optional; feature `otlp-grpc`).
    #[cfg(feature = "otlp-grpc")]
    OtlpGrpc(GrpcTraceExporter),
}

/// An OTLP/gRPC span exporter bound to the SDK-owned Tokio runtime that drives it.
///
/// Both fields are `Option` so `Drop` can release the exporter *before* the runtime, which is
/// the only safe order (the exporter's transport tasks live on that runtime).
#[cfg(feature = "otlp-grpc")]
pub(crate) struct GrpcTraceExporter {
    exporter: Option<opentelemetry_otlp::SpanExporter>,
    runtime: Option<crate::metric_exporter::GrpcRuntimeGuard>,
}

#[cfg(feature = "otlp-grpc")]
impl std::fmt::Debug for GrpcTraceExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTraceExporter").finish_non_exhaustive()
    }
}

#[cfg(feature = "otlp-grpc")]
impl GrpcTraceExporter {
    pub(crate) fn new(
        exporter: opentelemetry_otlp::SpanExporter,
        runtime: crate::metric_exporter::GrpcRuntimeGuard,
    ) -> Self {
        Self {
            exporter: Some(exporter),
            runtime: Some(runtime),
        }
    }

    fn exporter(&self) -> &opentelemetry_otlp::SpanExporter {
        self.exporter
            .as_ref()
            .expect("gRPC trace exporter is present until drop")
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("gRPC runtime is present until drop")
            .runtime()
    }
}

#[cfg(feature = "otlp-grpc")]
impl Drop for GrpcTraceExporter {
    fn drop(&mut self) {
        let runtime = self.runtime.take();
        drop(self.exporter.take());
        drop(runtime);
    }
}

// Dispatch the SpanExporter trait to the active variant.
impl SpanExporter for TraceExporterImpl {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Custom(inner) => inner.export(batch),
            #[cfg(feature = "otlp-http")]
            TraceExporterImpl::OtlpHttp(inner) => inner.export(batch).await,
            #[cfg(feature = "otlp-grpc")]
            TraceExporterImpl::OtlpGrpc(inner) => {
                if tokio::runtime::Handle::try_current().is_ok() {
                    return Err(OTelSdkError::InternalFailure(
                        "synchronous OTLP gRPC Traces export cannot call block_on from an \
                         entered Tokio runtime"
                            .to_owned(),
                    ));
                }
                inner.runtime().block_on(inner.exporter().export(batch))
            }
        }
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Custom(inner) => inner.shutdown(timeout),
            #[cfg(feature = "otlp-http")]
            TraceExporterImpl::OtlpHttp(inner) => inner.shutdown_with_timeout(timeout),
            #[cfg(feature = "otlp-grpc")]
            TraceExporterImpl::OtlpGrpc(inner) => inner.exporter().shutdown_with_timeout(timeout),
        }
    }
    fn force_flush(&self) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Custom(inner) => inner.force_flush(),
            #[cfg(feature = "otlp-http")]
            TraceExporterImpl::OtlpHttp(inner) => inner.force_flush(),
            #[cfg(feature = "otlp-grpc")]
            TraceExporterImpl::OtlpGrpc(inner) => inner.exporter().force_flush(),
        }
    }
    fn set_resource(&mut self, resource: &Resource) {
        match self {
            TraceExporterImpl::Custom(inner) => inner.set_resource(resource),
            #[cfg(feature = "otlp-http")]
            TraceExporterImpl::OtlpHttp(inner) => inner.set_resource(resource),
            #[cfg(feature = "otlp-grpc")]
            TraceExporterImpl::OtlpGrpc(inner) => {
                if let Some(exporter) = inner.exporter.as_mut() {
                    exporter.set_resource(resource);
                }
            }
        }
    }
}

/// Opaque trace-exporter handle. Owns a built `TraceExporterImpl` until it is consumed by a
/// span processor builder (via `set_exporter`) or destroyed.
#[repr(C)]
pub struct OtelTraceExporter {
    header: OtelHandleHeader,
    pub(crate) exporter: TraceExporterImpl,
}

impl OtelTraceExporter {
    pub(crate) fn new(exporter: TraceExporterImpl) -> Self {
        OtelTraceExporter {
            header: OtelHandleHeader::new(Self::KIND),
            exporter,
        }
    }
}

impl HasHandleHeader for OtelTraceExporter {
    const KIND: u64 = OTEL_HANDLE_KIND_TRACE_EXPORTER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Destroy a trace-exporter handle (no-op on NULL).
///
/// Do **not** call this on an exporter that was successfully transferred into a span
/// processor builder via `otel_batch_span_processor_builder_set_exporter` — the original
/// pointer is invalid after transfer and that builder owns the exporter.
///
/// # Safety
/// `exporter` must be NULL or a live exporter handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_trace_exporter_destroy(exporter: *mut OtelTraceExporter) {
    guard_unit(|| unsafe { destroy(exporter) });
}
