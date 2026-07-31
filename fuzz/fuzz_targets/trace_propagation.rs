#![no_main]

//! Fuzz W3C Trace Context extraction (traceparent + tracestate). Feeds arbitrary byte strings
//! as the two header values; any successfully extracted context is injected back and destroyed.
//! No arbitrary/unreadable pointers are constructed: only valid string views over owned buffers
//! are passed across the boundary.

use libfuzzer_sys::fuzz_target;
use opentelemetry_c_api::{
    otel_span_context_destroy, otel_trace_propagation_extract,
    otel_trace_propagation_inject_traceparent, otel_trace_propagation_inject_tracestate,
    OtelSpanContext, OtelStatus, OtelStringView,
};

fn view(bytes: &[u8]) -> OtelStringView {
    if bytes.is_empty() {
        OtelStringView::empty()
    } else {
        OtelStringView {
            ptr: bytes.as_ptr().cast(),
            len: bytes.len(),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    // Split the input into a traceparent slice and a tracestate slice on the first NUL byte.
    let (tp, ts) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[][..]),
    };

    let mut out: *mut OtelSpanContext = std::ptr::null_mut();
    // SAFETY: both views reference live borrowed slices for the duration of the call.
    let status = unsafe { otel_trace_propagation_extract(view(tp), view(ts), &mut out) };

    if status == OtelStatus::Ok {
        assert!(!out.is_null());
        // Round-trip inject: query length, then write into a right-sized buffer.
        let mut len = 0usize;
        // SAFETY: `out` is a live owned context; NULL buffer requests a length query.
        let q = unsafe {
            otel_trace_propagation_inject_traceparent(out, std::ptr::null_mut(), 0, &mut len)
        };
        assert_eq!(q, OtelStatus::Ok);
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` holds exactly `len` writable bytes.
        let w = unsafe {
            otel_trace_propagation_inject_traceparent(
                out,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(w, OtelStatus::Ok);
        assert_eq!(len, 55);

        let mut ts_len = 0usize;
        // SAFETY: NULL buffer requests a length query.
        let _ = unsafe {
            otel_trace_propagation_inject_tracestate(out, std::ptr::null_mut(), 0, &mut ts_len)
        };

        // SAFETY: `out` is a live owned context freed exactly once.
        unsafe { otel_span_context_destroy(out) };
    } else {
        assert!(out.is_null());
    }
});
