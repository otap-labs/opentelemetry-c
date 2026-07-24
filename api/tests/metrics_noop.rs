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

        macro_rules! sync {
            ($ty:ty, $create:ident, $record:ident, $destroy:ident, $value:expr_2021) => {{
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
