//! Hot-path FFI-overhead benchmarks for the **API-only, no-SDK** path.
//!
//! With no SDK installed the global provider resolves to the no-op default, so these
//! benchmarks measure the pure C API boundary cost — opaque handle allocation/validation,
//! the null-vtable no-op check, and panic-guarded dispatch — with **no** SDK work, OTel
//! object allocation, network, or collector. They guard the "API layer is thin" half of the
//! hot-path performance contract (see `opentelemetry-c/README.md`).
//!
//! Setup (handle acquisition) is kept out of the measured span/attribute loops by caching a
//! tracer and, for attribute setters, a span.
//!
//! Run with: `cargo bench -p opentelemetry-c-api`

use std::hint::black_box;
use std::os::raw::c_char;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use opentelemetry_c_api::{
    otel_counter_u64_add, otel_counter_u64_destroy, otel_gauge_f64_destroy, otel_gauge_f64_record,
    otel_global_meter_provider, otel_global_tracer_provider, otel_histogram_f64_destroy,
    otel_histogram_f64_record, otel_meter_create_f64_gauge, otel_meter_create_f64_histogram,
    otel_meter_create_u64_counter, otel_meter_destroy, otel_meter_provider_destroy,
    otel_meter_provider_get_meter, otel_span_destroy, otel_span_end, otel_span_set_bool_attribute,
    otel_span_set_double_attribute, otel_span_set_int64_attribute, otel_span_set_string_attribute,
    otel_tracer_destroy, otel_tracer_provider_destroy, otel_tracer_provider_get_tracer,
    otel_tracer_start_span, OtelAttributeType, OtelAttributeValue, OtelCounterU64, OtelGaugeF64,
    OtelHistogramF64, OtelKeyValue, OtelSpan, OtelStatus, OtelStringView, OtelTracer,
};

fn sv(s: &str) -> OtelStringView {
    OtelStringView {
        ptr: s.as_ptr().cast::<c_char>(),
        len: s.len(),
    }
}
fn empty() -> OtelStringView {
    OtelStringView {
        ptr: std::ptr::null(),
        len: 0,
    }
}

#[derive(Clone, Copy)]
enum AttributeShape {
    IntegerBool,
    MixedNumeric,
    String,
}

impl AttributeShape {
    fn name(self) -> &'static str {
        match self {
            Self::IntegerBool => "integer_bool",
            Self::MixedNumeric => "mixed_numeric",
            Self::String => "string",
        }
    }
}

struct AttributeSet {
    _keys: Vec<String>,
    _string_values: Vec<String>,
    attributes: Vec<OtelKeyValue>,
}

impl AttributeSet {
    fn new(shape: AttributeShape, count: usize) -> Self {
        let keys = (0..count)
            .map(|index| format!("benchmark.attribute.{index}"))
            .collect::<Vec<_>>();
        let string_values = match shape {
            AttributeShape::String => (0..count)
                .map(|index| format!("benchmark-value-{index:02}"))
                .collect(),
            _ => Vec::new(),
        };
        let attributes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let (value_type, value) = match shape {
                    AttributeShape::IntegerBool if index % 2 == 0 => (
                        OtelAttributeType::Int64 as u32,
                        OtelAttributeValue {
                            int64_value: i64::try_from(index).unwrap(),
                        },
                    ),
                    AttributeShape::IntegerBool => (
                        OtelAttributeType::Bool as u32,
                        OtelAttributeValue {
                            bool_value: u32::from(index % 2 == 0),
                        },
                    ),
                    AttributeShape::MixedNumeric if index % 3 == 0 => (
                        OtelAttributeType::Int64 as u32,
                        OtelAttributeValue {
                            int64_value: i64::try_from(index).unwrap(),
                        },
                    ),
                    AttributeShape::MixedNumeric if index % 3 == 1 => (
                        OtelAttributeType::Bool as u32,
                        OtelAttributeValue {
                            bool_value: u32::from(index % 2 == 0),
                        },
                    ),
                    AttributeShape::MixedNumeric => (
                        OtelAttributeType::Double as u32,
                        OtelAttributeValue {
                            double_value: index as f64 + 0.5,
                        },
                    ),
                    AttributeShape::String => (
                        OtelAttributeType::String as u32,
                        OtelAttributeValue {
                            string_value: sv(&string_values[index]),
                        },
                    ),
                };
                OtelKeyValue {
                    key: sv(key),
                    value_type,
                    value,
                }
            })
            .collect();
        Self {
            _keys: keys,
            _string_values: string_values,
            attributes,
        }
    }

    fn as_ffi(&self) -> (*const OtelKeyValue, usize) {
        if self.attributes.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (self.attributes.as_ptr(), self.attributes.len())
        }
    }
}

/// Acquire a (no-op) tracer via the global provider, for benchmarks that must not measure
/// tracer acquisition. `provider` is released immediately; the tracer handle stays valid.
fn cached_tracer() -> *mut OtelTracer {
    let provider = otel_global_tracer_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, sv("bench"), sv("0.1.0"), empty()) };
    unsafe { otel_tracer_provider_destroy(provider) };
    tracer
}

fn bench_api_no_sdk(c: &mut Criterion) {
    let mut g = c.benchmark_group("api_no_sdk");

    g.bench_function("global_provider_acquire", |b| {
        b.iter(|| {
            let p = otel_global_tracer_provider();
            black_box(p);
            unsafe { otel_tracer_provider_destroy(p) };
        });
    });

    g.bench_function("tracer_acquire", |b| {
        let provider = otel_global_tracer_provider();
        b.iter(|| {
            let t = unsafe {
                otel_tracer_provider_get_tracer(provider, sv("bench"), sv("0.1.0"), empty())
            };
            black_box(t);
            unsafe { otel_tracer_destroy(t) };
        });
        unsafe { otel_tracer_provider_destroy(provider) };
    });

    g.bench_function("start_end_span", |b| {
        let tracer = cached_tracer();
        b.iter(|| {
            let s: *mut OtelSpan =
                unsafe { otel_tracer_start_span(tracer, sv("op"), std::ptr::null()) };
            unsafe { otel_span_end(s) };
            unsafe { otel_span_destroy(s) };
        });
        unsafe { otel_tracer_destroy(tracer) };
    });

    g.bench_function("set_string_attribute", |b| {
        let tracer = cached_tracer();
        let span = unsafe { otel_tracer_start_span(tracer, sv("op"), std::ptr::null()) };
        b.iter(|| {
            let st = unsafe { otel_span_set_string_attribute(span, sv("http.method"), sv("GET")) };
            black_box(st);
        });
        unsafe {
            otel_span_end(span);
            otel_span_destroy(span);
            otel_tracer_destroy(tracer);
        }
    });

    g.bench_function("set_scalar_attributes", |b| {
        let tracer = cached_tracer();
        let span = unsafe { otel_tracer_start_span(tracer, sv("op"), std::ptr::null()) };
        b.iter(|| unsafe {
            black_box(otel_span_set_int64_attribute(
                span,
                sv("http.status_code"),
                200,
            ));
            black_box(otel_span_set_bool_attribute(span, sv("cache.hit"), 1));
            black_box(otel_span_set_double_attribute(span, sv("duration.ms"), 1.5));
        });
        unsafe {
            otel_span_end(span);
            otel_span_destroy(span);
            otel_tracer_destroy(tracer);
        }
    });

    g.bench_function("metrics_noop_counter_add_zero_attributes", |b| {
        let provider = otel_global_meter_provider();
        let meter =
            unsafe { otel_meter_provider_get_meter(provider, sv("bench"), empty(), empty()) };
        let mut counter: *mut OtelCounterU64 = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_meter_create_u64_counter(meter, sv("requests"), std::ptr::null(), &mut counter)
            },
            opentelemetry_c_api::OtelStatus::Ok
        );
        b.iter(|| {
            black_box(unsafe { otel_counter_u64_add(counter, 1, std::ptr::null(), 0) });
        });
        unsafe {
            otel_counter_u64_destroy(counter);
            otel_meter_destroy(meter);
            otel_meter_provider_destroy(provider);
        }
    });

    g.finish();

    let provider = otel_global_meter_provider();
    let meter = unsafe { otel_meter_provider_get_meter(provider, sv("bench"), empty(), empty()) };
    let mut counter: *mut OtelCounterU64 = std::ptr::null_mut();
    let mut gauge: *mut OtelGaugeF64 = std::ptr::null_mut();
    let mut histogram: *mut OtelHistogramF64 = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            otel_meter_create_u64_counter(meter, sv("requests"), std::ptr::null(), &mut counter)
        },
        OtelStatus::Ok
    );
    assert_eq!(
        unsafe { otel_meter_create_f64_gauge(meter, sv("depth"), std::ptr::null(), &mut gauge) },
        OtelStatus::Ok
    );
    assert_eq!(
        unsafe {
            otel_meter_create_f64_histogram(meter, sv("duration"), std::ptr::null(), &mut histogram)
        },
        OtelStatus::Ok
    );

    let mut attributes = c.benchmark_group("api_no_sdk_metrics_attributes");
    for shape in [
        AttributeShape::IntegerBool,
        AttributeShape::MixedNumeric,
        AttributeShape::String,
    ] {
        for count in [0, 1, 4, 8, 16] {
            let set = AttributeSet::new(shape, count);
            let (attribute_ptr, attribute_count) = set.as_ffi();
            attributes.bench_with_input(
                BenchmarkId::new(format!("counter_u64/{}", shape.name()), count),
                &set,
                |b, _| {
                    b.iter(|| {
                        black_box(unsafe {
                            otel_counter_u64_add(counter, 1, attribute_ptr, attribute_count)
                        });
                    });
                },
            );
            attributes.bench_with_input(
                BenchmarkId::new(format!("gauge_f64/{}", shape.name()), count),
                &set,
                |b, _| {
                    b.iter(|| {
                        black_box(unsafe {
                            otel_gauge_f64_record(gauge, 1.5, attribute_ptr, attribute_count)
                        });
                    });
                },
            );
            attributes.bench_with_input(
                BenchmarkId::new(format!("histogram_f64/{}", shape.name()), count),
                &set,
                |b, _| {
                    b.iter(|| {
                        black_box(unsafe {
                            otel_histogram_f64_record(
                                histogram,
                                1.5,
                                attribute_ptr,
                                attribute_count,
                            )
                        });
                    });
                },
            );
        }
    }
    attributes.finish();

    unsafe {
        otel_histogram_f64_destroy(histogram);
        otel_gauge_f64_destroy(gauge);
        otel_counter_u64_destroy(counter);
        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
    }
}

criterion_group!(benches, bench_api_no_sdk);
criterion_main!(benches);
