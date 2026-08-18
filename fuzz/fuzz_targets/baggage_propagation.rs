#![no_main]
// SPDX-License-Identifier: Apache-2.0

use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{OtelStatus, OtelStringView};
use opentelemetry_c_api::{
    otel_baggage_count, otel_baggage_destroy, otel_baggage_propagation_extract,
    otel_baggage_propagation_inject,
};

fuzz_target!(|data: &[u8]| {
    unsafe {
        let header = OtelStringView {
            ptr: data.as_ptr().cast(),
            len: data.len(),
        };
        let mut baggage = std::ptr::null_mut();
        let status = otel_baggage_propagation_extract(header, &mut baggage);
        if status != OtelStatus::Ok {
            assert!(baggage.is_null());
            return;
        }
        assert!(!baggage.is_null());
        assert!(otel_baggage_count(baggage) <= 64);

        let mut required = 0;
        assert_eq!(
            otel_baggage_propagation_inject(
                baggage,
                std::ptr::null_mut(),
                0,
                &mut required,
            ),
            OtelStatus::Ok
        );
        assert!(required <= 8192);
        let mut encoded = vec![0u8; required];
        assert_eq!(
            otel_baggage_propagation_inject(
                baggage,
                encoded.as_mut_ptr().cast(),
                encoded.len(),
                &mut required,
            ),
            OtelStatus::Ok
        );
        let mut roundtrip = std::ptr::null_mut();
        assert_eq!(
            otel_baggage_propagation_extract(
                OtelStringView {
                    ptr: encoded.as_ptr().cast(),
                    len: encoded.len(),
                },
                &mut roundtrip,
            ),
            OtelStatus::Ok
        );
        assert_eq!(otel_baggage_count(roundtrip), otel_baggage_count(baggage));
        otel_baggage_destroy(roundtrip);
        otel_baggage_destroy(baggage);
    }
});
