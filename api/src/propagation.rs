//! W3C Trace Context propagation: a bounded, direct `traceparent`/`tracestate` API.
//!
//! This module operates entirely on the API-owned immutable [`OtelSpanContext`]; it needs no
//! SDK, vtable entry, or global state. Extraction parses caller-supplied header values into a
//! new owned **remote** context; injection formats an existing context into caller-provided
//! buffers. No C pointer or callback state is retained past a call, and every input size is
//! bounded before allocation.
//!
//! Only the current W3C Trace Context Recommendation `traceparent`/`tracestate` fields are
//! handled here. Baggage is intentionally out of scope (see `TRACES_COMPLIANCE.md`).

use opentelemetry_c_abi::OtelStringView;

use crate::error::{clear_last_error, fail, OtelStatus};
use crate::handle::{checked_ref, guard_status, into_raw};
use crate::trace::OtelSpanContext;

/// Length of a version-`00` `traceparent`: `00-<32 hex>-<16 hex>-<2 hex>`.
const TRACEPARENT_LEN: usize = 55;
/// Upper bound on an accepted `tracestate` value, per the W3C 512-char recommendation with a
/// small margin. Longer input is rejected before allocation.
const MAX_TRACESTATE_LEN: usize = 512;
/// Upper bound on `tracestate` list members validated. The spec caps lists at 32 members.
const MAX_TRACESTATE_MEMBERS: usize = 32;

/// Decode exactly `N` bytes from `2*N` lowercase-hex ASCII characters.
///
/// Returns `None` on any non-lowercase-hex character or a length mismatch. Uppercase hex is
/// rejected to match the strict W3C requirement.
fn decode_hex<const N: usize>(input: &[u8]) -> Option<[u8; N]> {
    if input.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = decode_hex_nibble(input[2 * i])?;
        let lo = decode_hex_nibble(input[2 * i + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

fn decode_hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn encode_hex(bytes: &[u8], out: &mut String) {
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
}

/// Validate a `tracestate` value against a bounded subset of the W3C grammar.
///
/// Accepts a comma-separated list of `key=value` members (with optional surrounding
/// whitespace), rejecting: over-length input, too many members, empty keys, members without
/// `=`, and any byte outside printable ASCII (`0x20..=0x7e`). This is deliberately bounded;
/// it preserves the value verbatim once accepted.
fn validate_tracestate(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value.len() > MAX_TRACESTATE_LEN {
        return false;
    }
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return false;
    }
    let mut members = 0usize;
    for member in value.split(',') {
        let member = member.trim_matches(|c| c == ' ' || c == '\t');
        if member.is_empty() {
            // A trailing/leading empty segment (e.g. "a=1,") is malformed.
            return false;
        }
        members += 1;
        if members > MAX_TRACESTATE_MEMBERS {
            return false;
        }
        match member.split_once('=') {
            Some((key, _value)) if !key.is_empty() => {}
            _ => return false,
        }
    }
    true
}

/// Extract a remote [`OtelSpanContext`] from a W3C `traceparent` and optional `tracestate`.
///
/// `traceparent` is required. `tracestate` may be an empty view for none. On success `*out`
/// receives a new owned context with `is_remote == true`; release it with
/// `otel_span_context_destroy`. On any malformed input `*out` is set to NULL and a failure
/// status is returned with the last-error set. Trace-flag bits are preserved verbatim,
/// including unknown/reserved bits.
///
/// # Safety
/// The string views must satisfy the `otel_string_view_t` contract; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn otel_trace_propagation_extract(
    traceparent: OtelStringView,
    tracestate: OtelStringView,
    out: *mut *mut OtelSpanContext,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "output pointer is NULL");
        }
        // SAFETY: caller guarantees `out` is writable.
        unsafe { *out = std::ptr::null_mut() };

        // SAFETY: forwarded to the caller's contract.
        let tp = match unsafe { traceparent.as_str() } {
            Ok(s) => s.as_bytes(),
            Err(error) => return fail(error.status, error.message),
        };
        // Length + fixed-position separators.
        if tp.len() < TRACEPARENT_LEN {
            return fail(OtelStatus::InvalidArgument, "traceparent is too short");
        }
        if tp[2] != b'-' || tp[35] != b'-' || tp[52] != b'-' {
            return fail(
                OtelStatus::InvalidArgument,
                "traceparent has malformed separators",
            );
        }
        let version = match decode_hex::<1>(&tp[0..2]) {
            Some(v) => v[0],
            None => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "traceparent version is not hex",
                )
            }
        };
        if version == 0xff {
            return fail(
                OtelStatus::InvalidArgument,
                "traceparent version ff is invalid",
            );
        }
        if version == 0x00 && tp.len() != TRACEPARENT_LEN {
            return fail(
                OtelStatus::InvalidArgument,
                "version 00 traceparent must be exactly 55 characters",
            );
        }
        if tp.len() > TRACEPARENT_LEN && tp[TRACEPARENT_LEN] != b'-' {
            // A higher version may append fields, but only after a '-' delimiter.
            return fail(
                OtelStatus::InvalidArgument,
                "traceparent has trailing data without a delimiter",
            );
        }
        let trace_id = match decode_hex::<16>(&tp[3..35]) {
            Some(id) => id,
            None => return fail(OtelStatus::InvalidArgument, "trace-id is not lowercase hex"),
        };
        let span_id = match decode_hex::<8>(&tp[36..52]) {
            Some(id) => id,
            None => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "parent-id is not lowercase hex",
                )
            }
        };
        let flags = match decode_hex::<1>(&tp[53..55]) {
            Some(f) => f[0],
            None => return fail(OtelStatus::InvalidArgument, "trace-flags is not hex"),
        };
        if trace_id == [0u8; 16] {
            return fail(OtelStatus::InvalidArgument, "trace-id is all zero");
        }
        if span_id == [0u8; 8] {
            return fail(OtelStatus::InvalidArgument, "parent-id is all zero");
        }

        // Optional tracestate.
        // SAFETY: forwarded to the caller's contract.
        let ts = match unsafe { tracestate.as_str() } {
            Ok(s) => s,
            Err(error) => return fail(error.status, error.message),
        };
        if !validate_tracestate(ts) {
            return fail(OtelStatus::InvalidArgument, "tracestate is malformed");
        }
        let mut owned_ts = String::new();
        if owned_ts.try_reserve_exact(ts.len()).is_err() {
            return fail(OtelStatus::InternalError, "failed to allocate tracestate");
        }
        owned_ts.push_str(ts);

        let context = OtelSpanContext::from_parts(trace_id, span_id, flags, true, owned_ts);
        // SAFETY: caller guarantees `out` is writable.
        unsafe { *out = into_raw(context) };
        OtelStatus::Ok
    })
}

/// Shared length-query + write helper for the two injectors.
///
/// `render` produces the full formatted value into a fresh `String`. If `buffer` is NULL the
/// call is a pure length query (`*out_len` receives the required length, returns `Ok`). If
/// `buffer` is non-NULL and `capacity` is sufficient the bytes are written (no NUL) and
/// `*out_len` receives the count. An insufficient capacity returns `InvalidArgument` with
/// `*out_len` still set to the required length so the caller can resize and retry.
///
/// # Safety
/// `context` must satisfy the handle contract; `buffer` (if non-NULL) must be writable for
/// `capacity` bytes; `out_len` (if non-NULL) must be writable.
unsafe fn inject<F>(
    context: *const OtelSpanContext,
    buffer: *mut std::os::raw::c_char,
    capacity: usize,
    out_len: *mut usize,
    render: F,
) -> OtelStatus
where
    F: FnOnce(&OtelSpanContext, &mut String) -> Result<(), OtelStatus>,
{
    guard_status(|| {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        let context = match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => c,
            None => return OtelStatus::InvalidArgument,
        };
        if !context.is_valid() {
            return fail(OtelStatus::InvalidArgument, "span context is invalid");
        }
        let mut rendered = String::new();
        if let Err(status) = render(context, &mut rendered) {
            return status;
        }
        let required = rendered.len();
        if !out_len.is_null() {
            // SAFETY: caller guarantees `out_len` is writable when non-NULL.
            unsafe { *out_len = required };
        }
        if buffer.is_null() {
            // Pure length query.
            return OtelStatus::Ok;
        }
        if capacity < required {
            return fail(OtelStatus::InvalidArgument, "output buffer is too small");
        }
        // SAFETY: `buffer` is non-NULL and validated to hold at least `required` bytes; the
        // source and destination do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(rendered.as_ptr().cast(), buffer, required);
        }
        OtelStatus::Ok
    })
}

/// Format a version-`00` `traceparent` for `context` into `buffer`.
///
/// See [`inject`] for the buffer/length contract. The output is not NUL-terminated and
/// `*out_len` excludes any NUL. A version-`00` traceparent is always 55 bytes.
///
/// # Safety
/// See [`inject`].
#[no_mangle]
pub unsafe extern "C" fn otel_trace_propagation_inject_traceparent(
    context: *const OtelSpanContext,
    buffer: *mut std::os::raw::c_char,
    capacity: usize,
    out_len: *mut usize,
) -> OtelStatus {
    // SAFETY: forwarded to `inject`'s contract.
    unsafe {
        inject(context, buffer, capacity, out_len, |c, out| {
            let v = c.view();
            out.reserve(TRACEPARENT_LEN);
            out.push_str("00-");
            encode_hex(&v.trace_id, out);
            out.push('-');
            encode_hex(&v.span_id, out);
            out.push('-');
            encode_hex(&[v.trace_flags], out);
            Ok(())
        })
    }
}

/// Format the `tracestate` for `context` into `buffer`.
///
/// See [`inject`] for the buffer/length contract. An empty tracestate yields `*out_len == 0`
/// and writes nothing. The output is not NUL-terminated.
///
/// # Safety
/// See [`inject`].
#[no_mangle]
pub unsafe extern "C" fn otel_trace_propagation_inject_tracestate(
    context: *const OtelSpanContext,
    buffer: *mut std::os::raw::c_char,
    capacity: usize,
    out_len: *mut usize,
) -> OtelStatus {
    // SAFETY: forwarded to `inject`'s contract.
    unsafe {
        inject(context, buffer, capacity, out_len, |c, out| {
            let v = c.view();
            // SAFETY: `trace_state` in the view borrows the context's owned UTF-8 String.
            let ts = v.trace_state.as_str().map_err(|error| {
                crate::error::set_last_error(error.message);
                error.status
            })?;
            if out.try_reserve_exact(ts.len()).is_err() {
                crate::error::set_last_error("failed to allocate tracestate");
                return Err(OtelStatus::InternalError);
            }
            out.push_str(ts);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{
        otel_span_context_create, otel_span_context_destroy, otel_span_context_is_remote,
        otel_span_context_is_valid, otel_span_context_span_id, otel_span_context_trace_flags,
        otel_span_context_trace_id,
    };

    fn sv(s: &str) -> OtelStringView {
        OtelStringView {
            ptr: s.as_ptr().cast(),
            len: s.len(),
        }
    }

    fn empty() -> OtelStringView {
        OtelStringView::empty()
    }

    unsafe fn extract(tp: &str, ts: &str) -> *mut OtelSpanContext {
        let mut out: *mut OtelSpanContext = std::ptr::null_mut();
        let status = unsafe { otel_trace_propagation_extract(sv(tp), sv(ts), &mut out) };
        assert_eq!(status, OtelStatus::Ok, "expected Ok for {tp:?}");
        assert!(!out.is_null());
        out
    }

    unsafe fn inject_traceparent(ctx: *const OtelSpanContext) -> String {
        let mut len = 0usize;
        let q = unsafe {
            otel_trace_propagation_inject_traceparent(ctx, std::ptr::null_mut(), 0, &mut len)
        };
        assert_eq!(q, OtelStatus::Ok);
        let mut buf = vec![0u8; len];
        let w = unsafe {
            otel_trace_propagation_inject_traceparent(
                ctx,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(w, OtelStatus::Ok);
        String::from_utf8(buf).unwrap()
    }

    unsafe fn inject_tracestate(ctx: *const OtelSpanContext) -> String {
        let mut len = 0usize;
        let q = unsafe {
            otel_trace_propagation_inject_tracestate(ctx, std::ptr::null_mut(), 0, &mut len)
        };
        assert_eq!(q, OtelStatus::Ok);
        let mut buf = vec![0u8; len];
        if len > 0 {
            let w = unsafe {
                otel_trace_propagation_inject_tracestate(
                    ctx,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    &mut len,
                )
            };
            assert_eq!(w, OtelStatus::Ok);
        }
        String::from_utf8(buf).unwrap()
    }

    const VALID_TP: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn extracts_w3c_conformance_vector() {
        unsafe {
            let ctx = extract(VALID_TP, "");
            assert_eq!(otel_span_context_is_valid(ctx), 1);
            assert_eq!(otel_span_context_is_remote(ctx), 1);
            let mut tid = [0u8; 16];
            let mut sid = [0u8; 8];
            let mut flags = 0u8;
            otel_span_context_trace_id(ctx, tid.as_mut_ptr());
            otel_span_context_span_id(ctx, sid.as_mut_ptr());
            otel_span_context_trace_flags(ctx, &mut flags);
            assert_eq!(
                tid,
                [
                    0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e,
                    0x0e, 0x47, 0x36
                ]
            );
            assert_eq!(sid, [0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]);
            assert_eq!(flags, 0x01);
            otel_span_context_destroy(ctx);
        }
    }

    #[test]
    fn round_trips_traceparent() {
        unsafe {
            let ctx = extract(VALID_TP, "");
            assert_eq!(inject_traceparent(ctx), VALID_TP);
            otel_span_context_destroy(ctx);
        }
    }

    #[test]
    fn round_trips_tracestate() {
        unsafe {
            let ctx = extract(
                VALID_TP,
                "vendorname1=opaqueValue1,vendorname2=opaqueValue2",
            );
            assert_eq!(
                inject_tracestate(ctx),
                "vendorname1=opaqueValue1,vendorname2=opaqueValue2"
            );
            otel_span_context_destroy(ctx);
        }
    }

    #[test]
    fn unknown_flag_bits_survive_round_trip() {
        unsafe {
            // 0xff flags value in traceparent: reserved bits set, preserved verbatim.
            let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-ff";
            let ctx = extract(tp, "");
            let mut flags = 0u8;
            otel_span_context_trace_flags(ctx, &mut flags);
            assert_eq!(flags, 0xff);
            assert_eq!(inject_traceparent(ctx), tp);
            otel_span_context_destroy(ctx);
        }
    }

    #[test]
    fn accepts_higher_version_with_trailing_fields() {
        unsafe {
            // A future version may append fields after a '-' delimiter; first 55 bytes parse.
            let tp = "cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra";
            let ctx = extract(tp, "");
            assert_eq!(otel_span_context_is_valid(ctx), 1);
            otel_span_context_destroy(ctx);
        }
    }

    fn assert_rejected(tp: &str, ts: &str) {
        unsafe {
            let mut out: *mut OtelSpanContext = std::ptr::null_mut();
            let status = otel_trace_propagation_extract(sv(tp), sv(ts), &mut out);
            assert_eq!(
                status,
                OtelStatus::InvalidArgument,
                "expected reject for {tp:?}/{ts:?}"
            );
            assert!(out.is_null());
        }
    }

    #[test]
    fn rejects_malformed_traceparent() {
        // too short
        assert_rejected("00-abc", "");
        // bad separators
        assert_rejected(
            "00x4bf92f3577b34da6a3ce929d0e0e4736x00f067aa0ba902b7x01",
            "",
        );
        // version ff
        assert_rejected(
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "",
        );
        // uppercase hex is rejected (strict lowercase)
        assert_rejected(
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "",
        );
        // all-zero trace id
        assert_rejected(
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "",
        );
        // all-zero span id
        assert_rejected(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "",
        );
        // version 00 with trailing data (must be exactly 55)
        assert_rejected(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-x",
            "",
        );
        // higher version, trailing not delimited by '-'
        assert_rejected(
            "cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01x",
            "",
        );
        // non-hex flags
        assert_rejected(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-zz",
            "",
        );
    }

    #[test]
    fn rejects_malformed_tracestate() {
        // missing '='
        assert_rejected(VALID_TP, "novalue");
        // empty key
        assert_rejected(VALID_TP, "=v");
        // trailing empty member
        assert_rejected(VALID_TP, "a=1,");
        // control character
        assert_rejected(VALID_TP, "a=\u{0007}");
    }

    #[test]
    fn inject_undersized_buffer_reports_required_length() {
        unsafe {
            let ctx = extract(VALID_TP, "");
            let mut len = 0usize;
            let mut small = [0u8; 4];
            let status = otel_trace_propagation_inject_traceparent(
                ctx,
                small.as_mut_ptr().cast(),
                small.len(),
                &mut len,
            );
            assert_eq!(status, OtelStatus::InvalidArgument);
            assert_eq!(len, 55);
            otel_span_context_destroy(ctx);
        }
    }

    #[test]
    fn multi_hop_preserves_trace_id() {
        unsafe {
            // Simulate a hop: extract inbound, then form the outbound context reusing the same
            // trace id with a fresh span id; the injected traceparent keeps the trace id.
            let inbound = extract(VALID_TP, "");
            let mut tid = [0u8; 16];
            otel_span_context_trace_id(inbound, tid.as_mut_ptr());
            let child_span = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
            let outbound =
                otel_span_context_create(tid.as_ptr(), child_span.as_ptr(), 0x01, 0, empty());
            assert!(!outbound.is_null());
            let tp = inject_traceparent(outbound);
            assert!(tp.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-1122334455667788-01"));
            otel_span_context_destroy(inbound);
            otel_span_context_destroy(outbound);
        }
    }

    #[test]
    fn null_output_pointer_is_rejected() {
        unsafe {
            let status =
                otel_trace_propagation_extract(sv(VALID_TP), empty(), std::ptr::null_mut());
            assert_eq!(status, OtelStatus::InvalidArgument);
        }
    }

    #[test]
    fn validate_tracestate_bounds() {
        assert!(validate_tracestate(""));
        assert!(validate_tracestate("a=1"));
        assert!(validate_tracestate("a=1,b=2"));
        assert!(!validate_tracestate("a")); // no '='
        assert!(!validate_tracestate(&"a=1,".repeat(40))); // too many members
        assert!(!validate_tracestate(&"a".repeat(MAX_TRACESTATE_LEN + 1)));
    }
}
