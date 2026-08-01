//! The generic span-processor handle (`otel_span_processor_t`) and its internal implementation
//! enum.
//!
//! The opaque C handle wraps a `SpanProcessorImpl` — an internal enum whose variants are the
//! concrete span-processor kinds this SDK supports. It implements
//! [`opentelemetry_sdk::trace::SpanProcessor`], so the SDK builder stores a homogeneous
//! `Vec<SpanProcessorImpl>` and drives every processor uniformly. The batch and simple span
//! processors are the two variants today (both SDK core, always available). Adding another
//! processor kind is a new variant plus a constructor/builder — no change to the public C ABI,
//! the generic handle, or the SDK builder's storage.

use std::time::Duration;

use opentelemetry::Context;
use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_SPAN_PROCESSOR};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{
    BatchSpanProcessor, SimpleSpanProcessor, Span, SpanData, SpanProcessor,
};
use opentelemetry_sdk::Resource;

use crate::handle::{destroy, guard_unit, HasHandleHeader};
use crate::trace_exporter::TraceExporterImpl;

/// Internal span-processor implementation. Each variant is a concrete processor kind; the enum
/// dispatches the [`SpanProcessor`] trait to the active one. The batch processor is SDK core,
/// so this enum always has at least one variant.
#[derive(Debug)]
pub(crate) enum SpanProcessorImpl {
    /// Batch span processor (dedicated OS thread, spec-schedule export).
    Batch(BatchSpanProcessor),
    /// Simple span processor (synchronous export on the ending thread). Boxed to keep the enum
    /// small, as the simple processor embeds the exporter inline.
    Simple(Box<SimpleSpanProcessor<TraceExporterImpl>>),
}

impl SpanProcessor for SpanProcessorImpl {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        match self {
            SpanProcessorImpl::Batch(p) => p.on_start(span, cx),
            SpanProcessorImpl::Simple(p) => p.on_start(span, cx),
        }
    }
    fn on_end(&self, span: SpanData) {
        match self {
            SpanProcessorImpl::Batch(p) => p.on_end(span),
            SpanProcessorImpl::Simple(p) => p.on_end(span),
        }
    }
    fn force_flush(&self) -> OTelSdkResult {
        match self {
            SpanProcessorImpl::Batch(p) => p.force_flush(),
            SpanProcessorImpl::Simple(p) => p.force_flush(),
        }
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match self {
            SpanProcessorImpl::Batch(p) => p.shutdown_with_timeout(timeout),
            SpanProcessorImpl::Simple(p) => p.shutdown_with_timeout(timeout),
        }
    }
    fn set_resource(&mut self, resource: &Resource) {
        match self {
            SpanProcessorImpl::Batch(p) => p.set_resource(resource),
            SpanProcessorImpl::Simple(p) => p.set_resource(resource),
        }
    }
}

/// Opaque span-processor handle. Owns a built `SpanProcessorImpl` until it is consumed by the
/// SDK builder (via `add_span_processor`) or destroyed.
#[repr(C)]
pub struct OtelSpanProcessor {
    header: OtelHandleHeader,
    pub(crate) processor: SpanProcessorImpl,
}

impl OtelSpanProcessor {
    pub(crate) fn new(processor: SpanProcessorImpl) -> Self {
        OtelSpanProcessor {
            header: OtelHandleHeader::new(Self::KIND),
            processor,
        }
    }
}

impl HasHandleHeader for OtelSpanProcessor {
    const KIND: u64 = OTEL_HANDLE_KIND_SPAN_PROCESSOR;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Destroy a span-processor handle (no-op on NULL).
///
/// Do **not** call this on a processor that was successfully transferred into an SDK builder
/// via `otel_sdk_builder_add_span_processor` — the original pointer is invalid after transfer
/// and that builder owns the processor.
///
/// # Safety
/// `processor` must be NULL or a live processor handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_processor_destroy(processor: *mut OtelSpanProcessor) {
    guard_unit(|| unsafe { destroy(processor) });
}
