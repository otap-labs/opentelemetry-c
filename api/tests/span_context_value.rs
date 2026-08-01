//! API-only tests for the immutable `otel_span_context_t` value operations (Phase 2).
//!
//! These functions operate purely on the API-owned snapshot; no SDK is installed, so they
//! exercise construction, accessors, clone independence, NULL/wrong-kind handling, and the
//! reserved-flag and tracestate contracts without any backing implementation.

use opentelemetry_c_abi::{OtelStatus, OtelStringView};
use opentelemetry_c_api::{
    otel_global_tracer_provider, otel_span_context_clone, otel_span_context_create,
    otel_span_context_destroy, otel_span_context_is_remote, otel_span_context_is_valid,
    otel_span_context_span_id, otel_span_context_trace_flags, otel_span_context_trace_id,
    otel_span_context_tracestate, otel_tracer_provider_destroy, OtelSpanContext,
};

const TRACE_ID: [u8; 16] = [
    0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
];
const SPAN_ID: [u8; 8] = [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28];

fn view(s: &str) -> OtelStringView {
    OtelStringView {
        ptr: s.as_ptr().cast(),
        len: s.len(),
    }
}

unsafe fn create(
    trace_id: &[u8; 16],
    span_id: &[u8; 8],
    flags: u8,
    remote: u32,
    tracestate: &str,
) -> *mut OtelSpanContext {
    unsafe {
        otel_span_context_create(
            trace_id.as_ptr(),
            span_id.as_ptr(),
            flags,
            remote,
            view(tracestate),
        )
    }
}

#[test]
fn create_and_read_back_all_fields() {
    unsafe {
        let ctx = create(&TRACE_ID, &SPAN_ID, 0x01, 1, "vendor=abc");
        assert!(!ctx.is_null());
        assert_eq!(otel_span_context_is_valid(ctx), 1);
        assert_eq!(otel_span_context_is_remote(ctx), 1);

        let mut tid = [0u8; 16];
        let mut sid = [0u8; 8];
        let mut flags = 0u8;
        assert_eq!(
            otel_span_context_trace_id(ctx, tid.as_mut_ptr()),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_span_context_span_id(ctx, sid.as_mut_ptr()),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_span_context_trace_flags(ctx, &mut flags as *mut u8),
            OtelStatus::Ok
        );
        assert_eq!(tid, TRACE_ID);
        assert_eq!(sid, SPAN_ID);
        assert_eq!(flags, 0x01);

        let ts = otel_span_context_tracestate(ctx);
        let bytes = std::slice::from_raw_parts(ts.ptr.cast::<u8>(), ts.len);
        assert_eq!(bytes, b"vendor=abc");

        otel_span_context_destroy(ctx);
    }
}

#[test]
fn unknown_trace_flag_bits_are_preserved() {
    unsafe {
        // 0xFE sets several reserved bits and clears sampled; all must round-trip verbatim and
        // the context must remain valid.
        let ctx = create(&TRACE_ID, &SPAN_ID, 0xFE, 0, "");
        assert!(!ctx.is_null());
        assert_eq!(otel_span_context_is_valid(ctx), 1);
        let mut flags = 0u8;
        assert_eq!(
            otel_span_context_trace_flags(ctx, &mut flags as *mut u8),
            OtelStatus::Ok
        );
        assert_eq!(flags, 0xFE);
        assert_eq!(otel_span_context_is_remote(ctx), 0);
        otel_span_context_destroy(ctx);
    }
}

#[test]
fn empty_tracestate_is_empty_view() {
    unsafe {
        let ctx = create(&TRACE_ID, &SPAN_ID, 0, 0, "");
        assert!(!ctx.is_null());
        let ts = otel_span_context_tracestate(ctx);
        assert!(ts.ptr.is_null());
        assert_eq!(ts.len, 0);
        otel_span_context_destroy(ctx);
    }
}

#[test]
fn all_zero_ids_are_rejected() {
    unsafe {
        let zero16 = [0u8; 16];
        let zero8 = [0u8; 8];
        assert!(create(&zero16, &SPAN_ID, 0, 0, "").is_null());
        assert!(create(&TRACE_ID, &zero8, 0, 0, "").is_null());
    }
}

#[test]
fn invalid_utf8_tracestate_is_rejected() {
    unsafe {
        let bad = [0xffu8, 0xfe];
        let ts = OtelStringView {
            ptr: bad.as_ptr().cast(),
            len: bad.len(),
        };
        let ctx = otel_span_context_create(TRACE_ID.as_ptr(), SPAN_ID.as_ptr(), 0, 0, ts);
        assert!(ctx.is_null());
    }
}

#[test]
fn clone_is_independent_of_source() {
    unsafe {
        let src = create(&TRACE_ID, &SPAN_ID, 0x03, 1, "a=1");
        assert!(!src.is_null());
        let cloned = otel_span_context_clone(src);
        assert!(!cloned.is_null());
        // Destroy the source; the clone must remain fully readable.
        otel_span_context_destroy(src);

        let mut tid = [0u8; 16];
        assert_eq!(
            otel_span_context_trace_id(cloned, tid.as_mut_ptr()),
            OtelStatus::Ok
        );
        assert_eq!(tid, TRACE_ID);
        let ts = otel_span_context_tracestate(cloned);
        let bytes = std::slice::from_raw_parts(ts.ptr.cast::<u8>(), ts.len);
        assert_eq!(bytes, b"a=1");
        assert_eq!(otel_span_context_is_remote(cloned), 1);
        otel_span_context_destroy(cloned);
    }
}

#[test]
fn null_handles_fail_closed() {
    unsafe {
        let mut buf = [0u8; 16];
        assert_eq!(otel_span_context_is_valid(std::ptr::null()), 0);
        assert_eq!(otel_span_context_is_remote(std::ptr::null()), 0);
        assert_eq!(
            otel_span_context_trace_id(std::ptr::null(), buf.as_mut_ptr()),
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            otel_span_context_span_id(std::ptr::null(), buf.as_mut_ptr()),
            OtelStatus::InvalidArgument
        );
        let mut flags = 0u8;
        assert_eq!(
            otel_span_context_trace_flags(std::ptr::null(), &mut flags as *mut u8),
            OtelStatus::InvalidArgument
        );
        let ts = otel_span_context_tracestate(std::ptr::null());
        assert!(ts.ptr.is_null());
        assert_eq!(ts.len, 0);
    }
}

#[test]
fn null_output_buffers_are_rejected() {
    unsafe {
        let ctx = create(&TRACE_ID, &SPAN_ID, 0, 0, "");
        assert!(!ctx.is_null());
        assert_eq!(
            otel_span_context_trace_id(ctx, std::ptr::null_mut()),
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            otel_span_context_span_id(ctx, std::ptr::null_mut()),
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            otel_span_context_trace_flags(ctx, std::ptr::null_mut()),
            OtelStatus::InvalidArgument
        );
        otel_span_context_destroy(ctx);
    }
}

#[test]
fn wrong_kind_handle_is_rejected() {
    unsafe {
        // A live handle of a different kind (a tracer provider) must fail the kind check
        // rather than being read as a span context.
        let provider = otel_global_tracer_provider();
        assert!(!provider.is_null());
        let as_ctx = provider.cast::<OtelSpanContext>().cast_const();
        assert_eq!(otel_span_context_is_valid(as_ctx), 0);
        let mut tid = [0u8; 16];
        assert_eq!(
            otel_span_context_trace_id(as_ctx, tid.as_mut_ptr()),
            OtelStatus::InvalidArgument
        );
        otel_tracer_provider_destroy(provider);
    }
}
