#![no_main]
// SPDX-License-Identifier: Apache-2.0

//! Structured fuzzing of the versioned extended span-start surface
//! ([`otel_tracer_start_span_ex`]).
//!
//! The interesting attack surface is the versioned `otel_span_start_options_ex_t` descriptor:
//! the API must gate every optional field on `struct_size`, reject NULL arrays with non-zero
//! counts, enforce the reserved word and the parent/parent_context exclusion, and walk the
//! link array (each carrying its own context handle and attribute array) before handing a
//! borrowed [`OtelSpanStartConfig`] to the SDK, which reconstructs span contexts and links.
//!
//! Safety discipline: the fuzzer never supplies a raw address. Every pointer handed across the
//! ABI is NULL or points at a live Rust buffer / owned handle created in this function; only
//! lengths, tags, `struct_size`, counts, and structure fields are fuzzer-controlled. A NULL
//! pointer with a non-zero count is intentionally generated because the implementation must
//! reject it before any dereference — that is the property under test.

use std::mem::{offset_of, size_of};
use std::os::raw::c_char;
use std::ptr;
use std::sync::Once;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{OtelAttributeValue, OtelKeyValue, OtelStatus, OtelStringView};
use opentelemetry_c_api::{
    otel_global_tracer_provider, otel_span_context_create, otel_span_context_destroy,
    otel_span_destroy, otel_span_end, otel_tracer_destroy, otel_tracer_provider_destroy,
    otel_tracer_provider_get_tracer, otel_tracer_start_span_ex, OtelSpan, OtelSpanContext,
    OtelSpanLink, OtelSpanStartOptionsEx,
};
use opentelemetry_c_sdk::{
    otel_sdk_build, otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_set_as_global,
    OtelSdk, OtelSdkBuilder,
};

const MAX_STRING: usize = 48;
const MAX_ATTRIBUTES: usize = 8;
const MAX_LINKS: usize = 8;

/// An attribute array plus the string buffers backing its views.
type AttrArray = (Vec<OtelKeyValue>, Vec<(Vec<u8>, Vec<u8>)>);

#[derive(Arbitrary, Debug)]
struct CtxSpec {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    flags: u8,
    remote: bool,
    tracestate: Vec<u8>,
}

#[derive(Arbitrary, Debug)]
struct AttrSpec {
    key: Vec<u8>,
    value: Vec<u8>,
}

#[derive(Arbitrary, Debug)]
struct LinkSpec {
    ctx: CtxSpec,
    null_ctx: bool,
    lie_count: bool,
    attrs: Vec<AttrSpec>,
}

#[derive(Arbitrary, Debug)]
struct Input {
    name: Vec<u8>,
    struct_size_mode: u8,
    kind: u32,
    reserved: u32,
    start_time: u64,
    parent: Option<CtxSpec>,
    attrs: Vec<AttrSpec>,
    links: Vec<LinkSpec>,
    lie_attr_count: bool,
    lie_link_count: bool,
}

fn view(bytes: &[u8]) -> OtelStringView {
    if bytes.is_empty() {
        OtelStringView::empty()
    } else {
        OtelStringView {
            ptr: bytes.as_ptr().cast::<c_char>(),
            len: bytes.len().min(MAX_STRING),
        }
    }
}

/// Create an owned span-context handle from a spec (may be invalid/all-zero — that is fine,
/// the SDK must validate it). Returns NULL on rejection.
fn make_context(spec: &CtxSpec) -> *mut OtelSpanContext {
    let ts: Vec<u8> = spec.tracestate.iter().copied().take(MAX_STRING).collect();
    // SAFETY: trace_id/span_id are 16/8 readable bytes; tracestate view borrows `ts`.
    unsafe {
        otel_span_context_create(
            spec.trace_id.as_ptr(),
            spec.span_id.as_ptr(),
            spec.flags,
            u32::from(spec.remote),
            view(&ts),
        )
    }
}

/// Build an owned attribute array (string-valued) plus the backing string buffers, which the
/// caller must keep alive for the duration of the start-span call.
fn build_attrs(specs: &[AttrSpec]) -> AttrArray {
    let mut buffers: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for a in specs.iter().take(MAX_ATTRIBUTES) {
        let k: Vec<u8> = a.key.iter().copied().take(MAX_STRING).collect();
        let v: Vec<u8> = a.value.iter().copied().take(MAX_STRING).collect();
        buffers.push((k, v));
    }
    let kvs: Vec<OtelKeyValue> = buffers
        .iter()
        .map(|(k, v)| OtelKeyValue {
            key: view(k),
            value_type: 0, // string
            value: OtelAttributeValue {
                string_value: view(v),
            },
        })
        .collect();
    (kvs, buffers)
}

fn install_global_sdk() {
    // SAFETY: single builder → build → set-global sequence; owned handles freed as appropriate.
    unsafe {
        let builder: *mut OtelSdkBuilder = otel_sdk_builder_new();
        if builder.is_null() {
            return;
        }
        let mut sdk: *mut OtelSdk = ptr::null_mut();
        let status = otel_sdk_build(builder, &mut sdk);
        otel_sdk_builder_destroy(builder);
        if status != OtelStatus::Ok || sdk.is_null() {
            return;
        }
        // Intentionally leak the global SDK for the process lifetime of the fuzzer.
        let _ = otel_sdk_set_as_global(sdk);
    }
}

static INIT: Once = Once::new();

fuzz_target!(|input: Input| {
    INIT.call_once(install_global_sdk);

    let provider = otel_global_tracer_provider();
    if provider.is_null() {
        return;
    }
    let scope = b"fuzz";
    // SAFETY: `scope` is a live borrowed buffer; version/schema are empty views.
    let tracer = unsafe {
        otel_tracer_provider_get_tracer(
            provider,
            view(scope),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };

    // Backing storage that must outlive the call.
    let name: Vec<u8> = input.name.iter().copied().take(MAX_STRING).collect();
    let (top_attrs, _top_bufs) = build_attrs(&input.attrs);

    let parent_ctx = input.parent.as_ref().map(make_context);
    let parent_ptr = parent_ctx.unwrap_or(ptr::null_mut());

    // Build link array plus owned contexts and attribute buffers.
    let mut link_ctxs: Vec<*mut OtelSpanContext> = Vec::new();
    let mut link_attr_store: Vec<AttrArray> = Vec::new();
    let mut links: Vec<OtelSpanLink> = Vec::new();
    for spec in input.links.iter().take(MAX_LINKS) {
        let ctx = if spec.null_ctx {
            ptr::null_mut()
        } else {
            let c = make_context(&spec.ctx);
            link_ctxs.push(c);
            c
        };
        let (kvs, bufs) = build_attrs(&spec.attrs);
        let (attr_ptr, mut attr_count) = if kvs.is_empty() {
            (ptr::null(), 0usize)
        } else {
            (kvs.as_ptr(), kvs.len())
        };
        // Optionally lie: claim attributes exist while the pointer is NULL.
        let attr_ptr = if spec.lie_count {
            attr_count = attr_count.max(1);
            ptr::null()
        } else {
            attr_ptr
        };
        link_attr_store.push((kvs, bufs));
        links.push(OtelSpanLink {
            context: ctx,
            attributes: attr_ptr,
            attribute_count: attr_count,
        });
    }

    let (attr_ptr, mut attr_count) = if top_attrs.is_empty() {
        (ptr::null(), 0usize)
    } else {
        (top_attrs.as_ptr(), top_attrs.len())
    };
    let attr_ptr = if input.lie_attr_count {
        attr_count = attr_count.max(1);
        ptr::null()
    } else {
        attr_ptr
    };
    let (links_ptr, mut link_count) = if links.is_empty() {
        (ptr::null(), 0usize)
    } else {
        (links.as_ptr(), links.len())
    };
    let links_ptr = if input.lie_link_count {
        link_count = link_count.max(1);
        ptr::null()
    } else {
        links_ptr
    };

    let full = size_of::<OtelSpanStartOptionsEx>();
    let struct_size = match input.struct_size_mode % 5 {
        0 => full,
        1 => offset_of!(OtelSpanStartOptionsEx, attributes),
        2 => offset_of!(OtelSpanStartOptionsEx, attribute_count) + size_of::<usize>(),
        3 => 8,
        _ => full + 64,
    };

    let opts = OtelSpanStartOptionsEx {
        struct_size,
        kind: input.kind,
        reserved: input.reserved,
        parent: ptr::null(),
        parent_context: parent_ptr,
        start_time_unix_nanos: input.start_time,
        attributes: attr_ptr,
        attribute_count: attr_count,
        links: links_ptr,
        link_count,
    };

    // SAFETY: every pointer in `opts` is NULL or a live owned buffer/handle for this call.
    let span: *mut OtelSpan = unsafe { otel_tracer_start_span_ex(tracer, view(&name), &opts) };
    if !span.is_null() {
        // SAFETY: `span` is a live owned span.
        unsafe {
            otel_span_end(span);
            otel_span_destroy(span);
        }
    }

    // Clean up owned handles.
    unsafe {
        for c in link_ctxs {
            otel_span_context_destroy(c);
        }
        if let Some(c) = parent_ctx {
            otel_span_context_destroy(c);
        }
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }

    // Keep attribute backing storage alive until here.
    drop(link_attr_store);
});
