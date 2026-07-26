#![no_main]

use std::os::raw::c_char;
use std::ptr;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_api::{
    otel_counter_u64_add, otel_counter_u64_destroy, otel_global_meter_provider,
    otel_histogram_f64_destroy, otel_histogram_f64_record, otel_meter_create_f64_histogram,
    otel_meter_create_u64_counter, otel_meter_destroy, otel_meter_provider_destroy,
    otel_meter_provider_get_meter_with_options, OtelAttributeValue, OtelCounterU64,
    OtelHistogramF64, OtelInstrumentOptions, OtelKeyValue, OtelMeterOptions, OtelStatus,
    OtelStringView,
};

const MAX_STRING: usize = 64;
const MAX_ATTRIBUTES: usize = 8;
const MAX_BOUNDARIES: usize = 16;

#[derive(Arbitrary, Debug)]
struct Input {
    name: Vec<u8>,
    description: Vec<u8>,
    unit: Vec<u8>,
    value: Vec<u8>,
    string_mode: u8,
    struct_size: u64,
    value_type: u32,
    attribute_count_mode: u8,
    boundary_count_mode: u8,
    boundaries: Vec<u64>,
    number: u64,
}

fn view(bytes: &[u8], mode: u8) -> OtelStringView {
    let bytes = &bytes[..bytes.len().min(MAX_STRING)];
    match mode % 4 {
        0 => OtelStringView {
            ptr: bytes.as_ptr().cast::<c_char>(),
            len: bytes.len(),
        },
        1 => OtelStringView::empty(),
        2 => OtelStringView {
            ptr: ptr::null(),
            len: 1 + bytes.len(),
        },
        _ => OtelStringView {
            ptr: bytes.as_ptr().cast::<c_char>(),
            len: bytes.len().min(1),
        },
    }
}

fn prefix_size(raw: u64, complete: usize) -> u64 {
    match raw % 5 {
        0 => complete as u64,
        1 => 0,
        2 => complete.saturating_sub(1) as u64,
        3 => complete.saturating_add(1) as u64,
        _ => raw,
    }
}

fuzz_target!(|input: Input| {
    let name = view(&input.name, input.string_mode);
    let description = view(&input.description, input.string_mode.wrapping_add(1));
    let unit = view(&input.unit, input.string_mode.wrapping_add(2));
    let string_value = view(&input.value, input.string_mode.wrapping_add(3));

    let mut attributes = Vec::with_capacity(MAX_ATTRIBUTES);
    for index in 0..MAX_ATTRIBUTES {
        let value = match input.value_type % 5 {
            0 => OtelAttributeValue { string_value },
            1 => OtelAttributeValue {
                bool_value: input.number as u32,
            },
            2 => OtelAttributeValue {
                int64_value: input.number as i64,
            },
            _ => OtelAttributeValue {
                double_value: f64::from_bits(input.number),
            },
        };
        attributes.push(OtelKeyValue {
            key: if index % 2 == 0 {
                name
            } else {
                view(&input.description, input.string_mode)
            },
            value_type: input.value_type,
            value,
        });
    }

    let (attribute_ptr, attribute_count) = match input.attribute_count_mode % 3 {
        0 => (attributes.as_ptr(), input.name.len().min(MAX_ATTRIBUTES)),
        1 => (ptr::null(), 0),
        _ => (ptr::null(), usize::MAX),
    };
    let meter_options = OtelMeterOptions {
        struct_size: prefix_size(input.struct_size, std::mem::size_of::<OtelMeterOptions>()),
        name,
        version: description,
        schema_url: unit,
        attributes: attribute_ptr,
        attribute_count,
    };

    let boundary_values = input
        .boundaries
        .iter()
        .take(MAX_BOUNDARIES)
        .map(|bits| f64::from_bits(*bits))
        .collect::<Vec<_>>();
    let (boundary_ptr, boundary_count) = match input.boundary_count_mode % 3 {
        0 => (boundary_values.as_ptr(), boundary_values.len()),
        1 => (ptr::null(), 0),
        _ => (ptr::null(), usize::MAX),
    };
    let instrument_options = OtelInstrumentOptions {
        struct_size: prefix_size(
            input.struct_size.rotate_left(1),
            std::mem::size_of::<OtelInstrumentOptions>(),
        ),
        description,
        unit,
        boundaries: boundary_ptr,
        boundary_count,
    };

    let provider = otel_global_meter_provider();
    let meter = unsafe { otel_meter_provider_get_meter_with_options(provider, &meter_options) };
    if !meter.is_null() {
        let mut counter: *mut OtelCounterU64 = ptr::null_mut();
        let counter_status = unsafe {
            otel_meter_create_u64_counter(meter, name, &instrument_options, &mut counter)
        };
        if counter_status == OtelStatus::Ok {
            let _ = unsafe {
                otel_counter_u64_add(counter, input.number, attribute_ptr, attribute_count)
            };
        }
        unsafe { otel_counter_u64_destroy(counter) };

        let mut histogram: *mut OtelHistogramF64 = ptr::null_mut();
        let histogram_status = unsafe {
            otel_meter_create_f64_histogram(meter, name, &instrument_options, &mut histogram)
        };
        if histogram_status == OtelStatus::Ok {
            let _ = unsafe {
                otel_histogram_f64_record(
                    histogram,
                    f64::from_bits(input.number),
                    attribute_ptr,
                    attribute_count,
                )
            };
        }
        unsafe { otel_histogram_f64_destroy(histogram) };
        unsafe { otel_meter_destroy(meter) };
    }
    unsafe { otel_meter_provider_destroy(provider) };
});
