// SPDX-License-Identifier: Apache-2.0

use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use opentelemetry_c_api::*;

fn sv(value: &'static str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

fn empty() -> OtelStringView {
    OtelStringView::empty()
}

fn options() -> OtelInstrumentOptions {
    OtelInstrumentOptions {
        struct_size: std::mem::size_of::<OtelInstrumentOptions>() as u64,
        description: sv("description"),
        unit: sv("ms"),
        boundaries: std::ptr::null(),
        boundary_count: 0,
    }
}

fn last_error() -> String {
    let error = otel_last_error_message();
    assert!(!error.ptr.is_null());
    String::from_utf8(
        unsafe { std::slice::from_raw_parts(error.ptr.cast::<u8>(), error.len) }.to_vec(),
    )
    .unwrap()
}

extern "C" fn callback_u64(_observer: *mut OtelObserverU64, _data: *mut c_void) {}
extern "C" fn callback_i64(_observer: *mut OtelObserverI64, _data: *mut c_void) {}
extern "C" fn callback_f64(_observer: *mut OtelObserverF64, _data: *mut c_void) {}

static DESTROYED: AtomicUsize = AtomicUsize::new(0);
extern "C" fn destroy_data(_data: *mut c_void) {
    DESTROYED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn complete_noop_instrument_family_is_safe() {
    unsafe {
        let provider = otel_global_meter_provider();
        assert!(!provider.is_null());
        let meter = otel_meter_provider_get_meter(provider, sv("scope"), empty(), empty());
        assert!(!meter.is_null());
        let opts = options();

        #[allow(unknown_lints)]
        #[allow(edition_2024_expr_fragment_specifier)]
        macro_rules! sync {
            ($ty:ty, $create:ident, $record:ident, $destroy:ident, $value:expr) => {{
                let mut instrument: *mut $ty = std::ptr::null_mut();
                assert_eq!(
                    $create(meter, sv(stringify!($create)), &opts, &mut instrument),
                    OtelStatus::Ok
                );
                assert!(!instrument.is_null());
                // No-op recording returns before validating/converting attributes.
                assert_eq!(
                    $record(instrument, $value, std::ptr::null(), usize::MAX),
                    OtelStatus::Ok
                );
                $destroy(instrument);
            }};
        }

        sync!(
            OtelCounterU64,
            otel_meter_create_u64_counter,
            otel_counter_u64_add,
            otel_counter_u64_destroy,
            1
        );
        sync!(
            OtelCounterF64,
            otel_meter_create_f64_counter,
            otel_counter_f64_add,
            otel_counter_f64_destroy,
            1.0
        );
        let mut counter = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("bound_counter"), &opts, &mut counter),
            OtelStatus::Ok
        );
        let mut bound_counter = std::ptr::null_mut();
        // Like no-op recording, no-op binding does not inspect attributes.
        assert_eq!(
            otel_counter_u64_bind(counter, std::ptr::null(), usize::MAX, &mut bound_counter),
            OtelStatus::Ok
        );
        assert!(!bound_counter.is_null());
        assert_eq!(otel_bound_counter_u64_add(bound_counter, 3), OtelStatus::Ok);
        otel_counter_u64_destroy(counter);
        // The bound handle is independent of its source instrument handle.
        assert_eq!(otel_bound_counter_u64_add(bound_counter, 4), OtelStatus::Ok);
        otel_bound_counter_u64_destroy(bound_counter);
        sync!(
            OtelUpDownCounterI64,
            otel_meter_create_i64_up_down_counter,
            otel_up_down_counter_i64_add,
            otel_up_down_counter_i64_destroy,
            -1
        );
        sync!(
            OtelUpDownCounterF64,
            otel_meter_create_f64_up_down_counter,
            otel_up_down_counter_f64_add,
            otel_up_down_counter_f64_destroy,
            -1.0
        );
        sync!(
            OtelGaugeU64,
            otel_meter_create_u64_gauge,
            otel_gauge_u64_record,
            otel_gauge_u64_destroy,
            2
        );
        sync!(
            OtelGaugeI64,
            otel_meter_create_i64_gauge,
            otel_gauge_i64_record,
            otel_gauge_i64_destroy,
            -2
        );
        sync!(
            OtelGaugeF64,
            otel_meter_create_f64_gauge,
            otel_gauge_f64_record,
            otel_gauge_f64_destroy,
            2.0
        );

        let boundaries = [1.0, 2.0, 3.0];
        let histogram_options = OtelInstrumentOptions {
            boundaries: boundaries.as_ptr(),
            boundary_count: boundaries.len(),
            ..opts
        };
        let mut histogram_u64 = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_histogram(
                meter,
                sv("histogram_u64"),
                &histogram_options,
                &mut histogram_u64
            ),
            OtelStatus::Ok
        );
        otel_histogram_u64_destroy(histogram_u64);
        let mut histogram_f64 = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_f64_histogram(
                meter,
                sv("histogram_f64"),
                &histogram_options,
                &mut histogram_f64
            ),
            OtelStatus::Ok
        );
        let mut bound_histogram = std::ptr::null_mut();
        assert_eq!(
            otel_histogram_f64_bind(
                histogram_f64,
                std::ptr::null(),
                usize::MAX,
                &mut bound_histogram,
            ),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_bound_histogram_f64_record(bound_histogram, 2.5),
            OtelStatus::Ok
        );
        otel_bound_histogram_f64_destroy(bound_histogram);
        otel_histogram_f64_destroy(histogram_f64);

        macro_rules! observable {
            ($ty:ty, $create:ident, $destroy:ident, $callback:ident) => {{
                let mut instrument: *mut $ty = std::ptr::null_mut();
                assert_eq!(
                    $create(
                        meter,
                        sv(stringify!($create)),
                        &opts,
                        Some($callback),
                        std::ptr::null_mut(),
                        Some(destroy_data),
                        &mut instrument,
                    ),
                    OtelStatus::Ok
                );
                assert!(!instrument.is_null());
                $destroy(instrument);
            }};
        }

        DESTROYED.store(0, Ordering::SeqCst);
        observable!(
            OtelObservableCounterU64,
            otel_meter_create_u64_observable_counter,
            otel_observable_counter_u64_destroy,
            callback_u64
        );
        observable!(
            OtelObservableCounterF64,
            otel_meter_create_f64_observable_counter,
            otel_observable_counter_f64_destroy,
            callback_f64
        );
        observable!(
            OtelObservableUpDownCounterI64,
            otel_meter_create_i64_observable_up_down_counter,
            otel_observable_up_down_counter_i64_destroy,
            callback_i64
        );
        observable!(
            OtelObservableUpDownCounterF64,
            otel_meter_create_f64_observable_up_down_counter,
            otel_observable_up_down_counter_f64_destroy,
            callback_f64
        );
        observable!(
            OtelObservableGaugeU64,
            otel_meter_create_u64_observable_gauge,
            otel_observable_gauge_u64_destroy,
            callback_u64
        );
        observable!(
            OtelObservableGaugeI64,
            otel_meter_create_i64_observable_gauge,
            otel_observable_gauge_i64_destroy,
            callback_i64
        );
        observable!(
            OtelObservableGaugeF64,
            otel_meter_create_f64_observable_gauge,
            otel_observable_gauge_f64_destroy,
            callback_f64
        );
        assert_eq!(DESTROYED.load(Ordering::SeqCst), 7);

        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
    }
}

#[test]
fn invalid_configuration_is_rejected_even_without_sdk() {
    unsafe {
        let provider = otel_global_meter_provider();
        let meter = otel_meter_provider_get_meter(provider, sv("scope"), empty(), empty());
        let mut counter = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("_invalid"), std::ptr::null(), &mut counter),
            OtelStatus::InvalidConfig
        );
        assert!(counter.is_null());
        let short_options = std::mem::size_of::<u64>() as u64;
        assert_eq!(
            otel_meter_create_u64_counter(
                meter,
                sv("short_options"),
                (&short_options as *const u64).cast(),
                &mut counter,
            ),
            OtelStatus::InvalidConfig
        );
        assert!(counter.is_null());
        let invalid = [1.0, f64::NAN];
        let opts = OtelInstrumentOptions {
            struct_size: std::mem::size_of::<OtelInstrumentOptions>() as u64,
            description: empty(),
            unit: empty(),
            boundaries: invalid.as_ptr(),
            boundary_count: invalid.len(),
        };
        let mut histogram = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_f64_histogram(meter, sv("hist"), &opts, &mut histogram),
            OtelStatus::InvalidConfig
        );
        assert!(histogram.is_null());
        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
    }
}

#[test]
fn instrument_options_prefix_and_input_boundaries_are_validated() {
    unsafe {
        let provider = otel_global_meter_provider();
        let meter = otel_meter_provider_get_meter(provider, sv("scope"), empty(), empty());
        let mut counter = std::ptr::null_mut();

        let mut oversized = options();
        oversized.struct_size += 32;
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("oversized"), &oversized, &mut counter),
            OtelStatus::Ok
        );
        otel_counter_u64_destroy(counter);

        let valid_name = "a".repeat(255);
        let valid_name = OtelStringView {
            ptr: valid_name.as_ptr().cast(),
            len: valid_name.len(),
        };
        counter = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_counter(meter, valid_name, std::ptr::null(), &mut counter),
            OtelStatus::Ok
        );
        otel_counter_u64_destroy(counter);

        let invalid_name = "a".repeat(256);
        let invalid_name = OtelStringView {
            ptr: invalid_name.as_ptr().cast(),
            len: invalid_name.len(),
        };
        counter = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_counter(meter, invalid_name, std::ptr::null(), &mut counter),
            OtelStatus::InvalidConfig
        );
        assert!(counter.is_null());

        let unit_63 = "u".repeat(63);
        let mut opts = options();
        opts.unit = OtelStringView {
            ptr: unit_63.as_ptr().cast(),
            len: unit_63.len(),
        };
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("unit_63"), &opts, &mut counter),
            OtelStatus::Ok
        );
        otel_counter_u64_destroy(counter);

        let unit_64 = "u".repeat(64);
        opts.unit = OtelStringView {
            ptr: unit_64.as_ptr().cast(),
            len: unit_64.len(),
        };
        counter = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("unit_64"), &opts, &mut counter),
            OtelStatus::InvalidConfig
        );
        assert!(counter.is_null());

        let invalid_utf8 = [0xff_u8];
        opts = options();
        opts.description = OtelStringView {
            ptr: invalid_utf8.as_ptr().cast(),
            len: invalid_utf8.len(),
        };
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("bad_description"), &opts, &mut counter),
            OtelStatus::InvalidUtf8
        );
        opts = options();
        opts.unit = OtelStringView {
            ptr: invalid_utf8.as_ptr().cast(),
            len: invalid_utf8.len(),
        };
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("bad_unit"), &opts, &mut counter),
            OtelStatus::InvalidUtf8
        );

        opts = options();
        opts.boundary_count = 1;
        let mut histogram = std::ptr::null_mut();
        assert_eq!(
            otel_meter_create_f64_histogram(meter, sv("null_bounds"), &opts, &mut histogram),
            OtelStatus::InvalidArgument
        );
        opts.boundary_count = 65_537;
        assert_eq!(
            otel_meter_create_f64_histogram(meter, sv("many_bounds"), &opts, &mut histogram),
            OtelStatus::InvalidConfig
        );

        let boundary = 1.0;
        opts = options();
        opts.boundaries = &boundary;
        assert_eq!(
            otel_meter_create_u64_counter(meter, sv("non_hist_bounds"), &opts, &mut counter),
            OtelStatus::InvalidConfig
        );

        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
    }
}

#[test]
fn complete_scope_options_are_validated_without_an_sdk() {
    unsafe {
        let provider = otel_global_meter_provider();
        let attributes = [
            OtelKeyValue {
                key: sv("component"),
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: sv("checkout"),
                },
            },
            OtelKeyValue {
                key: sv("stable"),
                value_type: OtelAttributeType::Bool as u32,
                value: OtelAttributeValue { bool_value: 1 },
            },
        ];
        let options = OtelMeterOptions {
            struct_size: std::mem::size_of::<OtelMeterOptions>() as u64,
            name: sv("scope"),
            version: sv("1.2.3"),
            schema_url: sv("https://example.test/schema"),
            attributes: attributes.as_ptr(),
            attribute_count: attributes.len(),
        };
        let meter = otel_meter_provider_get_meter_with_options(provider, &options);
        assert!(!meter.is_null());
        otel_meter_destroy(meter);

        let empty_options = OtelMeterOptions {
            struct_size: std::mem::size_of::<OtelMeterOptions>() as u64,
            name: empty(),
            version: empty(),
            schema_url: empty(),
            attributes: std::ptr::null(),
            attribute_count: 0,
        };
        let meter = otel_meter_provider_get_meter_with_options(provider, &empty_options);
        assert!(!meter.is_null());
        otel_meter_destroy(meter);

        let duplicates = [attributes[0], attributes[0]];
        let duplicate_options = OtelMeterOptions {
            attributes: duplicates.as_ptr(),
            attribute_count: duplicates.len(),
            ..options
        };
        assert!(otel_meter_provider_get_meter_with_options(provider, &duplicate_options).is_null());
        assert!(last_error().contains("duplicate scope attribute key"));

        let null_attributes = OtelMeterOptions {
            attributes: std::ptr::null(),
            attribute_count: 1,
            ..options
        };
        assert!(otel_meter_provider_get_meter_with_options(provider, &null_attributes).is_null());
        assert!(last_error().contains("NULL with non-zero count"));

        let short_size = std::mem::size_of::<u64>() as u64;
        assert!(otel_meter_provider_get_meter_with_options(
            provider,
            (&short_size as *const u64).cast()
        )
        .is_null());
        assert!(last_error().contains("struct_size"));

        let invalid_utf8 = [0xff_u8];
        let invalid_name = OtelMeterOptions {
            name: OtelStringView {
                ptr: invalid_utf8.as_ptr().cast(),
                len: invalid_utf8.len(),
            },
            ..options
        };
        assert!(otel_meter_provider_get_meter_with_options(provider, &invalid_name).is_null());
        assert!(last_error().contains("UTF-8"));

        otel_meter_provider_destroy(provider);
    }
}
