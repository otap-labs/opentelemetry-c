// SPDX-License-Identifier: Apache-2.0

//! Hot-path FFI-overhead benchmarks for the **SDK-backed** path.
//!
//! Requires the `otlp` cargo feature (part of default features); the `[[bench]]` target sets
//! `required-features = ["otlp"]`, so it is skipped in `--no-default-features` builds.
//!
//! These install a *real* Rust OTel SDK trace pipeline (OTLP exporter + batch span processor)
//! as the global provider through the public C SDK API, then drive the public C API span/tracer
//! entrypoints. They measure the true cost of a span/attribute/event through the C boundary plus
//! the Rust SDK's own machinery — the counterpart to the no-op `api_hotpath` bench.
//!
//! No collector is required: the OTLP exporter targets a closed loopback port (`127.0.0.1:1`),
//! so background export attempts may fail fast (connection refused) and are discarded. The batch
//! processor's bounded queue/buffer keeps memory bounded; a very large scheduled delay makes
//! flushing batch-size-driven rather than timer-driven, and `force_flush` is never called. This
//! is **not** an exporter/network throughput benchmark and is not a default regression guard for
//! export.
//!
//! Setup (pipeline build + global install + tracer acquisition) is kept out of the measured loops.
//! Attribute/event setters run on a **fresh** span per iteration via `iter_batched`, so the span's
//! per-span attribute/event limits are never hit and each op measures the real "store" path; the
//! excluded setup/teardown starts and ends+destroys that span.
//!
//! Run with: `cargo bench -p opentelemetry-c-sdk` (default features include `otlp`).

use std::hint::black_box;
use std::os::raw::c_char;
use std::ptr;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use opentelemetry::metrics::MeterProvider;
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{ManualReader, SdkMeterProvider};

// Public C API entrypoints (dev-dep): the real process-global provider slot and span/tracer ops.
use opentelemetry_c_api::{
    otel_bound_counter_u64_add, otel_bound_counter_u64_destroy, otel_bound_histogram_f64_destroy,
    otel_bound_histogram_f64_record, otel_counter_u64_add, otel_counter_u64_bind,
    otel_counter_u64_destroy, otel_gauge_f64_destroy, otel_gauge_f64_record,
    otel_global_meter_provider, otel_global_tracer_provider, otel_histogram_f64_bind,
    otel_histogram_f64_destroy, otel_histogram_f64_record, otel_meter_create_f64_gauge,
    otel_meter_create_f64_histogram, otel_meter_create_u64_counter, otel_meter_destroy,
    otel_meter_provider_destroy, otel_meter_provider_get_meter, otel_span_add_event,
    otel_span_destroy, otel_span_end, otel_span_set_bool_attribute, otel_span_set_double_attribute,
    otel_span_set_int64_attribute, otel_span_set_string_attribute, otel_tracer_destroy,
    otel_tracer_provider_destroy, otel_tracer_provider_get_tracer, otel_tracer_start_span,
    OtelAttributeType, OtelAttributeValue, OtelBoundCounterU64, OtelBoundHistogramF64,
    OtelCounterU64, OtelGaugeF64, OtelHistogramF64, OtelKeyValue, OtelMeter, OtelSpan, OtelStatus,
    OtelStringView, OtelTracer,
};
// Public C SDK entrypoints (crate under bench): build the pipeline and install it as global.
use opentelemetry_c_sdk::{
    otel_batch_span_processor_builder_build, otel_batch_span_processor_builder_destroy,
    otel_batch_span_processor_builder_new, otel_batch_span_processor_builder_set_exporter,
    otel_batch_span_processor_builder_set_max_export_batch_size,
    otel_batch_span_processor_builder_set_max_queue_size,
    otel_batch_span_processor_builder_set_scheduled_delay_millis,
    otel_otlp_metric_exporter_builder_build, otel_otlp_metric_exporter_builder_destroy,
    otel_otlp_metric_exporter_builder_new, otel_otlp_metric_exporter_builder_set_endpoint,
    otel_otlp_trace_exporter_builder_build, otel_otlp_trace_exporter_builder_destroy,
    otel_otlp_trace_exporter_builder_new, otel_otlp_trace_exporter_builder_set_endpoint,
    otel_periodic_metric_reader_builder_build, otel_periodic_metric_reader_builder_destroy,
    otel_periodic_metric_reader_builder_new, otel_periodic_metric_reader_builder_set_exporter,
    otel_periodic_metric_reader_builder_set_interval_millis, otel_sdk_build,
    otel_sdk_builder_add_metric_reader, otel_sdk_builder_add_span_processor,
    otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_builder_set_service_name,
    otel_sdk_destroy, otel_sdk_metrics_shutdown, otel_sdk_set_as_global,
    otel_sdk_set_metrics_as_global, otel_sdk_shutdown, OtelMetricExporter,
    OtelPeriodicMetricReader, OtelSdk, OtelSpanProcessor, OtelTraceExporter,
};

fn sv(s: &str) -> OtelStringView {
    OtelStringView {
        ptr: s.as_ptr().cast::<c_char>(),
        len: s.len(),
    }
}
fn empty() -> OtelStringView {
    OtelStringView {
        ptr: ptr::null(),
        len: 0,
    }
}
fn assert_ok(status: OtelStatus) {
    assert_eq!(status, OtelStatus::Ok, "setup FFI call failed: {status:?}");
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

struct CAttributeSet {
    _keys: Vec<String>,
    _string_values: Vec<String>,
    attributes: Vec<OtelKeyValue>,
}

impl CAttributeSet {
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
            (ptr::null(), 0)
        } else {
            (self.attributes.as_ptr(), self.attributes.len())
        }
    }
}

fn rust_attributes(shape: AttributeShape, count: usize) -> Vec<KeyValue> {
    (0..count)
        .map(|index| {
            let key = format!("benchmark.attribute.{index}");
            match shape {
                AttributeShape::IntegerBool if index % 2 == 0 => {
                    KeyValue::new(key, i64::try_from(index).unwrap())
                }
                AttributeShape::IntegerBool => KeyValue::new(key, index % 2 == 0),
                AttributeShape::MixedNumeric if index % 3 == 0 => {
                    KeyValue::new(key, i64::try_from(index).unwrap())
                }
                AttributeShape::MixedNumeric if index % 3 == 1 => {
                    KeyValue::new(key, index % 2 == 0)
                }
                AttributeShape::MixedNumeric => KeyValue::new(key, index as f64 + 0.5),
                AttributeShape::String => KeyValue::new(key, format!("benchmark-value-{index:02}")),
            }
        })
        .collect()
}

/// Build a real trace pipeline and install it as the process-global provider. Returns the owned
/// SDK handle (shut down + destroyed at the end of the bench). No collector is required: the
/// OTLP exporter targets a closed loopback port, so background export attempts may fail fast
/// (connection refused) and are discarded.
fn install_sdk() -> *mut OtelSdk {
    unsafe {
        // OTLP exporter. `build` does not connect (the client is lazy); only a batch flush would,
        // and it fails fast against the closed port with no collector running.
        let xb = otel_otlp_trace_exporter_builder_new();
        assert_ok(otel_otlp_trace_exporter_builder_set_endpoint(
            xb,
            sv("http://127.0.0.1:1/v1/traces"),
        ));
        let mut exporter: *mut OtelTraceExporter = ptr::null_mut();
        assert_ok(otel_otlp_trace_exporter_builder_build(xb, &mut exporter));
        otel_otlp_trace_exporter_builder_destroy(xb);

        // Batch processor. Bounded queue/batch keep memory bounded; a very large scheduled delay
        // keeps flushing batch-size-driven so no timer-driven export fires during the bench.
        let pb = otel_batch_span_processor_builder_new();
        assert_ok(otel_batch_span_processor_builder_set_exporter(pb, exporter));
        assert_ok(otel_batch_span_processor_builder_set_max_queue_size(
            pb, 8192,
        ));
        assert_ok(otel_batch_span_processor_builder_set_max_export_batch_size(
            pb, 2048,
        ));
        assert_ok(otel_batch_span_processor_builder_set_scheduled_delay_millis(pb, 3_600_000));
        let mut processor: *mut OtelSpanProcessor = ptr::null_mut();
        assert_ok(otel_batch_span_processor_builder_build(pb, &mut processor));
        otel_batch_span_processor_builder_destroy(pb);

        // SDK + global install.
        let sb = otel_sdk_builder_new();
        assert_ok(otel_sdk_builder_set_service_name(sb, sv("otel-c-bench")));
        assert_ok(otel_sdk_builder_add_span_processor(sb, processor));

        let meb = otel_otlp_metric_exporter_builder_new();
        assert_ok(otel_otlp_metric_exporter_builder_set_endpoint(
            meb,
            sv("http://127.0.0.1:1/v1/metrics"),
        ));
        let mut metric_exporter: *mut OtelMetricExporter = ptr::null_mut();
        assert_ok(otel_otlp_metric_exporter_builder_build(
            meb,
            &mut metric_exporter,
        ));
        otel_otlp_metric_exporter_builder_destroy(meb);
        let mrb = otel_periodic_metric_reader_builder_new();
        assert_ok(otel_periodic_metric_reader_builder_set_interval_millis(
            mrb, 3_600_000,
        ));
        assert_ok(otel_periodic_metric_reader_builder_set_exporter(
            mrb,
            metric_exporter,
        ));
        let mut metric_reader: *mut OtelPeriodicMetricReader = ptr::null_mut();
        assert_ok(otel_periodic_metric_reader_builder_build(
            mrb,
            &mut metric_reader,
        ));
        otel_periodic_metric_reader_builder_destroy(mrb);
        assert_ok(otel_sdk_builder_add_metric_reader(sb, metric_reader));

        let mut sdk: *mut OtelSdk = ptr::null_mut();
        assert_ok(otel_sdk_build(sb, &mut sdk));
        otel_sdk_builder_destroy(sb);

        assert_ok(otel_sdk_set_as_global(sdk));
        assert_ok(otel_sdk_set_metrics_as_global(sdk));
        sdk
    }
}

/// Acquire a tracer through the installed global provider (SDK-backed). Used by span benches that
/// should not measure tracer acquisition.
fn global_tracer() -> *mut OtelTracer {
    let provider = otel_global_tracer_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, sv("bench"), sv("0.1.0"), empty()) };
    unsafe { otel_tracer_provider_destroy(provider) };
    assert!(!tracer.is_null(), "SDK-backed tracer acquisition failed");
    tracer
}

/// RAII guard that ends and destroys a span outside the measured region (used as the excluded
/// teardown of `iter_batched`, so the timed routine measures only the setter/event op).
struct SpanGuard(*mut OtelSpan);
impl Drop for SpanGuard {
    fn drop(&mut self) {
        unsafe {
            otel_span_end(self.0);
            otel_span_destroy(self.0);
        }
    }
}

fn bench_sdk_backed(c: &mut Criterion) {
    let sdk = install_sdk();
    let tracer = global_tracer();
    let start = |t: *mut OtelTracer| unsafe { otel_tracer_start_span(t, sv("op"), ptr::null()) };

    let mut g = c.benchmark_group("sdk_backed");

    g.bench_function("tracer_acquire_global", |b| {
        b.iter(|| {
            let provider = otel_global_tracer_provider();
            let t = unsafe {
                otel_tracer_provider_get_tracer(provider, sv("bench"), sv("0.1.0"), empty())
            };
            black_box(t);
            unsafe {
                otel_tracer_destroy(t);
                otel_tracer_provider_destroy(provider);
            }
        });
    });

    g.bench_function("start_end_span", |b| {
        b.iter(|| {
            let s = start(tracer);
            unsafe {
                otel_span_end(s);
                otel_span_destroy(s);
            }
        });
    });

    g.bench_function("set_string_attribute", |b| {
        b.iter_batched(
            || SpanGuard(start(tracer)),
            |guard| {
                let st = unsafe {
                    otel_span_set_string_attribute(guard.0, sv("http.method"), sv("GET"))
                };
                black_box(st);
                guard
            },
            BatchSize::SmallInput,
        );
    });

    g.bench_function("set_scalar_attributes", |b| {
        b.iter_batched(
            || SpanGuard(start(tracer)),
            |guard| {
                unsafe {
                    black_box(otel_span_set_int64_attribute(
                        guard.0,
                        sv("http.status_code"),
                        200,
                    ));
                    black_box(otel_span_set_bool_attribute(guard.0, sv("cache.hit"), 1));
                    black_box(otel_span_set_double_attribute(
                        guard.0,
                        sv("duration.ms"),
                        1.5,
                    ));
                }
                guard
            },
            BatchSize::SmallInput,
        );
    });

    g.bench_function("add_event_bounded_attrs", |b| {
        // A fixed, small ("bounded") set of event attributes built once outside the loop.
        let attrs = [
            OtelKeyValue {
                key: sv("http.method"),
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: sv("GET"),
                },
            },
            OtelKeyValue {
                key: sv("http.status_code"),
                value_type: OtelAttributeType::Int64 as u32,
                value: OtelAttributeValue { int64_value: 200 },
            },
            OtelKeyValue {
                key: sv("cache.hit"),
                value_type: OtelAttributeType::Bool as u32,
                value: OtelAttributeValue { bool_value: 1 },
            },
        ];
        b.iter_batched(
            || SpanGuard(start(tracer)),
            |guard| {
                let st = unsafe {
                    otel_span_add_event(guard.0, sv("request"), attrs.as_ptr(), attrs.len())
                };
                black_box(st);
                guard
            },
            BatchSize::SmallInput,
        );
    });

    let meter_provider = otel_global_meter_provider();
    let meter: *mut OtelMeter =
        unsafe { otel_meter_provider_get_meter(meter_provider, sv("bench"), empty(), empty()) };
    let mut counter: *mut OtelCounterU64 = ptr::null_mut();
    let mut gauge: *mut OtelGaugeF64 = ptr::null_mut();
    let mut histogram: *mut OtelHistogramF64 = ptr::null_mut();
    assert_ok(unsafe {
        otel_meter_create_u64_counter(meter, sv("requests"), ptr::null(), &mut counter)
    });
    assert_ok(unsafe {
        otel_meter_create_f64_gauge(meter, sv("queue_depth"), ptr::null(), &mut gauge)
    });
    assert_ok(unsafe {
        otel_meter_create_f64_histogram(meter, sv("request_duration"), ptr::null(), &mut histogram)
    });

    g.bench_function("metrics_counter_add_zero_attributes", |b| {
        b.iter(|| {
            black_box(unsafe { otel_counter_u64_add(counter, 1, ptr::null(), 0) });
        });
    });
    g.bench_function("metrics_gauge_record_zero_attributes", |b| {
        b.iter(|| {
            black_box(unsafe { otel_gauge_f64_record(gauge, 1.5, ptr::null(), 0) });
        });
    });
    g.bench_function("metrics_histogram_record_zero_attributes", |b| {
        b.iter(|| {
            black_box(unsafe { otel_histogram_f64_record(histogram, 1.5, ptr::null(), 0) });
        });
    });

    g.finish();

    let mut attributes = c.benchmark_group("sdk_backed_metrics_attributes");
    for shape in [
        AttributeShape::IntegerBool,
        AttributeShape::MixedNumeric,
        AttributeShape::String,
    ] {
        for count in [0, 1, 4, 8, 16] {
            let set = CAttributeSet::new(shape, count);
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
            let mut bound_counter: *mut OtelBoundCounterU64 = ptr::null_mut();
            assert_ok(unsafe {
                otel_counter_u64_bind(counter, attribute_ptr, attribute_count, &mut bound_counter)
            });
            let mut bound_histogram: *mut OtelBoundHistogramF64 = ptr::null_mut();
            assert_ok(unsafe {
                otel_histogram_f64_bind(
                    histogram,
                    attribute_ptr,
                    attribute_count,
                    &mut bound_histogram,
                )
            });
            attributes.bench_with_input(
                BenchmarkId::new(format!("bound_counter_u64/{}", shape.name()), count),
                &set,
                |b, _| {
                    b.iter(|| {
                        black_box(unsafe { otel_bound_counter_u64_add(bound_counter, 1) });
                    });
                },
            );
            attributes.bench_with_input(
                BenchmarkId::new(format!("bound_histogram_f64/{}", shape.name()), count),
                &set,
                |b, _| {
                    b.iter(|| {
                        black_box(unsafe { otel_bound_histogram_f64_record(bound_histogram, 1.5) });
                    });
                },
            );
            unsafe {
                otel_bound_counter_u64_destroy(bound_counter);
                otel_bound_histogram_f64_destroy(bound_histogram);
            }
        }
    }
    attributes.finish();

    let rust_provider = SdkMeterProvider::builder()
        .with_reader(ManualReader::builder().build())
        .build();
    let rust_meter = rust_provider.meter("bench-direct");
    let rust_counter = rust_meter.u64_counter("requests").build();
    let rust_gauge = rust_meter.f64_gauge("depth").build();
    let rust_histogram = rust_meter.f64_histogram("duration").build();
    let mut direct = c.benchmark_group("rust_sdk_metrics_attributes");
    for shape in [
        AttributeShape::IntegerBool,
        AttributeShape::MixedNumeric,
        AttributeShape::String,
    ] {
        for count in [0, 1, 4, 8, 16] {
            let rust_attributes = rust_attributes(shape, count);
            direct.bench_with_input(
                BenchmarkId::new(format!("counter_u64/{}", shape.name()), count),
                &rust_attributes,
                |b, attributes| {
                    b.iter(|| rust_counter.add(black_box(1), black_box(attributes)));
                },
            );
            direct.bench_with_input(
                BenchmarkId::new(format!("gauge_f64/{}", shape.name()), count),
                &rust_attributes,
                |b, attributes| {
                    b.iter(|| rust_gauge.record(black_box(1.5), black_box(attributes)));
                },
            );
            direct.bench_with_input(
                BenchmarkId::new(format!("histogram_f64/{}", shape.name()), count),
                &rust_attributes,
                |b, attributes| {
                    b.iter(|| rust_histogram.record(black_box(1.5), black_box(attributes)));
                },
            );
            let bound_counter = rust_counter.bind(&rust_attributes);
            direct.bench_with_input(
                BenchmarkId::new(format!("bound_counter_u64/{}", shape.name()), count),
                &bound_counter,
                |b, instrument| {
                    b.iter(|| instrument.add(black_box(1)));
                },
            );
            let bound_histogram = rust_histogram.bind(&rust_attributes);
            direct.bench_with_input(
                BenchmarkId::new(format!("bound_histogram_f64/{}", shape.name()), count),
                &bound_histogram,
                |b, instrument| {
                    b.iter(|| instrument.record(black_box(1.5)));
                },
            );
        }
    }
    direct.finish();
    drop(rust_counter);
    drop(rust_gauge);
    drop(rust_histogram);
    drop(rust_meter);
    let _ = rust_provider.shutdown();

    // Teardown (not measured): drop the cached tracer, then shut down and destroy the SDK.
    unsafe {
        otel_histogram_f64_destroy(histogram);
        otel_gauge_f64_destroy(gauge);
        otel_counter_u64_destroy(counter);
        otel_meter_destroy(meter);
        otel_meter_provider_destroy(meter_provider);
        otel_tracer_destroy(tracer);
        otel_sdk_metrics_shutdown(sdk, 2_000);
        otel_sdk_shutdown(sdk, 2_000);
        otel_sdk_destroy(sdk);
    }
}

criterion_group!(benches, bench_sdk_backed);
criterion_main!(benches);
