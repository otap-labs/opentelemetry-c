//! The simple span processor constructor (`otel_simple_span_processor_create`).
//!
//! Consumes an [`OtelTraceExporter`] and produces a generic [`OtelSpanProcessor`] handle
//! wrapping a `SimpleSpanProcessor`. Unlike the batch processor, a simple processor has no
//! configuration and no worker thread: it exports each finished span synchronously on the
//! thread that ended it, serializing ending threads behind one exporter. It is intended for
//! tests, short-lived programs, and debugging; production pipelines should prefer the batch
//! processor.
//!
//! Ownership: on `OTEL_STATUS_OK` the exporter transfers into the processor and the original
//! pointer becomes invalid; on failure the caller still owns it.

use opentelemetry_sdk::trace::SimpleSpanProcessor;

use opentelemetry_c_abi::OtelStatus;

use crate::error::{clear_last_error, fail};
use crate::handle::{guard_status, into_raw, take};
use crate::span_processor::{OtelSpanProcessor, SpanProcessorImpl};
use crate::trace_exporter::OtelTraceExporter;

/// Build a **simple** span processor that takes ownership of `exporter`.
///
/// A simple processor exports synchronously on the thread that ended each span, so it
/// serializes ending threads behind one exporter. It is intended for tests, short-lived
/// programs, and debugging; production pipelines should prefer the batch span processor.
///
/// On `OTEL_STATUS_OK` ownership of `exporter` transfers and the original pointer becomes
/// invalid. On failure the caller still owns `exporter`. On success `*out` receives a new
/// [`OtelSpanProcessor`] handle owned by the caller (release with `otel_span_processor_destroy`
/// or transfer it into the SDK builder via `otel_sdk_builder_add_span_processor`).
///
/// # Safety
/// `out` must address writable storage; `exporter` must be NULL or a live trace-exporter
/// handle not used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_simple_span_processor_create(
    exporter: *mut OtelTraceExporter,
    out: *mut *mut OtelSpanProcessor,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        // Take ownership only after every other argument has been validated, so a rejected
        // call always leaves the exporter caller-owned.
        let owned = match unsafe { take::<OtelTraceExporter>(exporter) } {
            Some(owned) => owned,
            None => return OtelStatus::InvalidArgument,
        };
        let processor =
            SpanProcessorImpl::Simple(Box::new(SimpleSpanProcessor::new(owned.exporter)));
        unsafe { *out = into_raw(OtelSpanProcessor::new(processor)) };
        OtelStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "otlp-http")]
    use crate::otlp_exporter::{
        otel_otlp_trace_exporter_builder_build, otel_otlp_trace_exporter_builder_destroy,
        otel_otlp_trace_exporter_builder_new, otel_otlp_trace_exporter_builder_set_endpoint,
    };
    #[cfg(feature = "otlp-http")]
    use crate::span_processor::otel_span_processor_destroy;
    #[cfg(feature = "otlp-http")]
    use opentelemetry_c_abi::OtelStringView;

    #[cfg(feature = "otlp-http")]
    fn sv(s: &str) -> OtelStringView {
        OtelStringView {
            ptr: s.as_ptr().cast::<std::os::raw::c_char>(),
            len: s.len(),
        }
    }

    /// Build a valid exporter handle (constructing it does not connect).
    #[cfg(feature = "otlp-http")]
    fn make_exporter() -> *mut OtelTraceExporter {
        unsafe {
            let eb = otel_otlp_trace_exporter_builder_new();
            assert_eq!(
                otel_otlp_trace_exporter_builder_set_endpoint(
                    eb,
                    sv("http://127.0.0.1:9/v1/traces")
                ),
                OtelStatus::Ok
            );
            let mut exporter: *mut OtelTraceExporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_trace_exporter_builder_build(eb, &mut exporter),
                OtelStatus::Ok
            );
            otel_otlp_trace_exporter_builder_destroy(eb);
            assert!(!exporter.is_null());
            exporter
        }
    }

    #[test]
    fn create_rejects_null_out_and_leaves_exporter_owned() {
        unsafe {
            // A NULL out pointer is rejected without consuming the (also NULL) exporter.
            assert_eq!(
                otel_simple_span_processor_create(std::ptr::null_mut(), std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
        }
    }

    #[test]
    fn create_rejects_null_exporter() {
        unsafe {
            let mut processor: *mut OtelSpanProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_simple_span_processor_create(std::ptr::null_mut(), &mut processor),
                OtelStatus::InvalidArgument
            );
            assert!(processor.is_null());
        }
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn create_takes_ownership_only_on_success() {
        unsafe {
            let exporter = make_exporter();
            let mut processor: *mut OtelSpanProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_simple_span_processor_create(exporter, &mut processor),
                OtelStatus::Ok
            );
            assert!(!processor.is_null());
            // The exporter moved into the processor; destroying the processor releases it.
            otel_span_processor_destroy(processor);
        }
    }

    /// A simple processor wired into a full SDK must drive its `SpanProcessor` dispatch
    /// (set_resource during build, force_flush, shutdown) without panicking. The OTLP endpoint
    /// is intentionally unreachable — export failures are swallowed by the pipeline; this
    /// exercises the `SpanProcessorImpl::Simple` forwarding paths end to end.
    #[cfg(feature = "otlp-http")]
    #[test]
    fn simple_processor_drives_full_sdk_lifecycle() {
        use crate::sdk::{
            otel_sdk_build, otel_sdk_builder_add_span_processor, otel_sdk_builder_destroy,
            otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_force_flush, otel_sdk_shutdown,
            OtelSdk,
        };
        unsafe {
            let exporter = make_exporter();
            let mut processor: *mut OtelSpanProcessor = std::ptr::null_mut();
            assert_eq!(
                otel_simple_span_processor_create(exporter, &mut processor),
                OtelStatus::Ok
            );

            let builder = otel_sdk_builder_new();
            assert_eq!(
                otel_sdk_builder_add_span_processor(builder, processor),
                OtelStatus::Ok
            );
            let mut sdk: *mut OtelSdk = std::ptr::null_mut();
            assert_eq!(otel_sdk_build(builder, &mut sdk), OtelStatus::Ok);
            assert!(!sdk.is_null());
            otel_sdk_builder_destroy(builder);

            assert_eq!(otel_sdk_force_flush(sdk, 1000), OtelStatus::Ok);
            assert_eq!(otel_sdk_shutdown(sdk, 1000), OtelStatus::Ok);
            otel_sdk_destroy(sdk);
        }
    }
}
