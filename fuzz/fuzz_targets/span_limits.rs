#![no_main]

//! Structured fuzzing of the versioned span-limits surface
//! ([`otel_sdk_builder_set_span_limits`]).
//!
//! The attack surface is the versioned `otel_span_limits_t` descriptor: the SDK must gate on
//! `struct_size`, enforce the reserved word, and accept any `u32` bound (including 0) without
//! panicking before storing the resolved `SpanLimits` on the builder.
//!
//! Safety discipline: the fuzzer never supplies a raw address. The only pointer handed across
//! the ABI is a reference to a live `OtelSpanLimits` on this function's stack; every scalar
//! field is fuzzer-controlled. `struct_size` only gates which fields are read — the backing
//! struct is always fully valid memory — so an arbitrary `struct_size` can never cause an
//! out-of-bounds read.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
// The SDK cdylib resolves its `otel_api_*` global-registration imports against the API crate;
// pull it in so those symbols are available at link time.
use opentelemetry_c_api as _;
use opentelemetry_c_sdk::{
    otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_builder_set_span_limits,
    OtelSpanLimits,
};

#[derive(Arbitrary, Debug)]
struct Input {
    struct_size: usize,
    max_attributes_per_span: u32,
    max_events_per_span: u32,
    max_links_per_span: u32,
    max_attributes_per_event: u32,
    max_attributes_per_link: u32,
    reserved: u32,
    null_config: bool,
}

fuzz_target!(|input: Input| {
    let builder = otel_sdk_builder_new();
    if builder.is_null() {
        return;
    }

    // Clamp struct_size to a sane window around the real struct so the fuzzer spends its budget
    // on the meaningful gating boundary rather than absurd magnitudes; the value only controls
    // which fields are read, never how much memory is touched.
    let max_size = std::mem::size_of::<OtelSpanLimits>() * 4;
    let config = OtelSpanLimits {
        struct_size: input.struct_size % (max_size + 1),
        max_attributes_per_span: input.max_attributes_per_span,
        max_events_per_span: input.max_events_per_span,
        max_links_per_span: input.max_links_per_span,
        max_attributes_per_event: input.max_attributes_per_event,
        max_attributes_per_link: input.max_attributes_per_link,
        reserved: input.reserved,
    };

    unsafe {
        if input.null_config {
            let _ = otel_sdk_builder_set_span_limits(builder, std::ptr::null());
        }
        let _ = otel_sdk_builder_set_span_limits(builder, &config);
        otel_sdk_builder_destroy(builder);
    }
});
