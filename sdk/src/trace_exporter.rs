//! The generic trace-exporter handle (`otel_trace_exporter_t`) and its internal
//! implementation enum.
//!
//! The opaque C handle wraps a `TraceExporterImpl` — an internal enum whose variants are the
//! concrete exporter kinds this SDK supports. It implements
//! [`opentelemetry_sdk::trace::SpanExporter`], so a span processor can drive it uniformly
//! regardless of which exporter is inside. The OTLP HTTP/protobuf exporter is one **optional**
//! variant (feature `otlp-http`), not SDK core: with `--no-default-features` the enum has no
//! variants and the SDK core still builds. Adding a new exporter kind is a new variant plus a
//! builder — no change to the public C ABI or handle shape.

use std::time::Duration;

use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_TRACE_EXPORTER};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::Resource;

use crate::handle::{destroy, guard_unit, HasHandleHeader};

/// Internal trace-exporter implementation. Each variant is a concrete exporter kind; the enum
/// dispatches the [`SpanExporter`] trait to the active one. OTLP is optional (`otlp-http`);
/// with no exporter feature enabled the enum is uninhabited and cannot be constructed.
#[derive(Debug)]
pub(crate) enum TraceExporterImpl {
    /// OTLP HTTP/protobuf exporter (optional; feature `otlp-http`).
    #[cfg(feature = "otlp-http")]
    Otlp(opentelemetry_otlp::SpanExporter),
}

// Dispatch the SpanExporter trait to the active variant. Split by feature so the OTLP-disabled
// build (an uninhabited enum) is handled without an unreachable placeholder variant.
#[cfg(feature = "otlp-http")]
impl SpanExporter for TraceExporterImpl {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Otlp(inner) => inner.export(batch).await,
        }
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Otlp(inner) => inner.shutdown_with_timeout(timeout),
        }
    }
    fn force_flush(&self) -> OTelSdkResult {
        match self {
            TraceExporterImpl::Otlp(inner) => inner.force_flush(),
        }
    }
    fn set_resource(&mut self, resource: &Resource) {
        match self {
            TraceExporterImpl::Otlp(inner) => inner.set_resource(resource),
        }
    }
}

#[cfg(not(feature = "otlp-http"))]
impl SpanExporter for TraceExporterImpl {
    async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
        // Uninhabited when no exporter feature is enabled: cannot be constructed or called.
        match *self {}
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        match *self {}
    }
    fn force_flush(&self) -> OTelSdkResult {
        match *self {}
    }
    fn set_resource(&mut self, _resource: &Resource) {
        match *self {}
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
/// processor builder via `otel_batch_span_processor_builder_set_exporter` — that builder owns
/// it now (a transferred handle's magic is poisoned, so this degrades to a safe no-op).
///
/// # Safety
/// `exporter` must be NULL or a live exporter handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_trace_exporter_destroy(exporter: *mut OtelTraceExporter) {
    guard_unit(|| unsafe { destroy(exporter) });
}
