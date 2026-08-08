#![no_main]
// SPDX-License-Identifier: Apache-2.0

//! Structured fuzzing of the built-in sampler configuration surface
//! ([`otel_sdk_builder_set_sampler`]).
//!
//! The interesting attack surface is the versioned `otel_sampler_config_t` descriptor: the SDK
//! must gate the parent-based fields on `struct_size`, enforce the reserved words, bound the
//! ratio to `[0, 1]` for ratio-based (root) samplers, and reject a parent-based root that is
//! itself parent-based — all before storing a resolved sampler on the builder.
//!
//! Safety discipline: the fuzzer never supplies a raw address. The only pointer handed across
//! the ABI is a reference to a live `OtelSamplerConfig` on this function's stack; every scalar
//! field (`struct_size`, `sampler_type`, `reserved`, `ratio`, root type) is fuzzer-controlled.
//! `struct_size` only gates which fields the implementation reads — the backing struct is
//! always fully valid memory — so an arbitrary `struct_size` can never cause an out-of-bounds
//! read.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
// The SDK cdylib resolves its `otel_api_*` global-registration imports against the API crate;
// pull it in so those symbols are available at link time.
use opentelemetry_c_api as _;
use opentelemetry_c_sdk::{
    otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_builder_set_sampler, OtelSamplerConfig,
};

#[derive(Arbitrary, Debug)]
struct Input {
    struct_size: usize,
    sampler_type: u32,
    reserved: u32,
    ratio: f64,
    parent_based_root_type: u32,
    reserved2: u32,
    null_config: bool,
}

fuzz_target!(|input: Input| {
    let builder = otel_sdk_builder_new();
    if builder.is_null() {
        return;
    }

    // Clamp struct_size to a sane window around the real struct so the fuzzer spends its
    // budget on the meaningful gating boundaries rather than absurd magnitudes; the value only
    // controls which fields are read, never how much memory is touched.
    let max_size = std::mem::size_of::<OtelSamplerConfig>() * 4;
    let config = OtelSamplerConfig {
        struct_size: input.struct_size % (max_size + 1),
        sampler_type: input.sampler_type,
        reserved: input.reserved,
        ratio: input.ratio,
        parent_based_root_type: input.parent_based_root_type,
        reserved2: input.reserved2,
    };

    unsafe {
        if input.null_config {
            let _ = otel_sdk_builder_set_sampler(builder, std::ptr::null());
        }
        // The implementation must return a status for every input and never panic or read out
        // of bounds; both accepted and rejected configs leave the builder destroyable.
        let _ = otel_sdk_builder_set_sampler(builder, &config);
        otel_sdk_builder_destroy(builder);
    }
});
