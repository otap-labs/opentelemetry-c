#![no_main]

use std::os::raw::c_char;
use std::ptr;

use libfuzzer_sys::fuzz_target;
use opentelemetry_c_api::{
    otel_counter_u64_add, otel_counter_u64_destroy, otel_gauge_f64_destroy, otel_gauge_f64_record,
    otel_global_meter_provider, otel_meter_create_f64_gauge, otel_meter_create_u64_counter,
    otel_meter_destroy, otel_meter_provider_destroy, otel_meter_provider_get_meter, OtelCounterU64,
    OtelGaugeF64, OtelMeter, OtelMeterProvider, OtelStatus, OtelStringView,
};

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

fuzz_target!(|data: &[u8]| {
    let provider = otel_global_meter_provider();
    let meter = unsafe { otel_meter_provider_get_meter(provider, sv("fuzz"), sv(""), sv("")) };
    if meter.is_null() {
        unsafe { otel_meter_provider_destroy(provider) };
        return;
    }

    let mut counter: *mut OtelCounterU64 = ptr::null_mut();
    let mut gauge: *mut OtelGaugeF64 = ptr::null_mut();
    if unsafe { otel_meter_create_u64_counter(meter, sv("counter"), ptr::null(), &mut counter) }
        != OtelStatus::Ok
        || unsafe { otel_meter_create_f64_gauge(meter, sv("gauge"), ptr::null(), &mut gauge) }
            != OtelStatus::Ok
    {
        unsafe {
            otel_counter_u64_destroy(counter);
            otel_gauge_f64_destroy(gauge);
            otel_meter_destroy(meter);
            otel_meter_provider_destroy(provider);
        }
        return;
    }

    for byte in data.iter().take(32) {
        match byte % 6 {
            0 => {
                let _ = unsafe { otel_counter_u64_add(counter, *byte as u64, ptr::null(), 0) };
            }
            1 => {
                let _ = unsafe { otel_gauge_f64_record(gauge, *byte as f64, ptr::null(), 0) };
            }
            2 => {
                let wrong = gauge.cast::<OtelCounterU64>();
                let _ = unsafe { otel_counter_u64_add(wrong, *byte as u64, ptr::null(), 0) };
            }
            3 => {
                let wrong = counter.cast::<OtelGaugeF64>();
                let _ = unsafe { otel_gauge_f64_record(wrong, *byte as f64, ptr::null(), 0) };
            }
            4 => {
                let wrong = meter.cast::<OtelMeterProvider>();
                let transient =
                    unsafe { otel_meter_provider_get_meter(wrong, sv("wrong"), sv(""), sv("")) };
                unsafe { otel_meter_destroy(transient) };
            }
            _ => {
                let wrong = provider.cast::<OtelMeter>();
                let mut transient = ptr::null_mut();
                let _ = unsafe {
                    otel_meter_create_u64_counter(wrong, sv("wrong"), ptr::null(), &mut transient)
                };
                unsafe { otel_counter_u64_destroy(transient) };
            }
        }
    }

    unsafe {
        otel_counter_u64_destroy(counter);
        otel_gauge_f64_destroy(gauge);
        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
    }
});
