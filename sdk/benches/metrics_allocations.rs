//! Steady-state allocation counts for synchronous Metrics recording.
//!
//! The timed Criterion suites remain the source for latency. This executable uses a counting
//! allocator around only the recording calls and reports allocations and allocated bytes per
//! operation for the API no-SDK path, the public C SDK-backed path, and direct Rust SDK calls.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use opentelemetry::metrics::MeterProvider;
use opentelemetry::KeyValue;
use opentelemetry_c_api::{
    otel_bound_counter_u64_add, otel_bound_counter_u64_destroy, otel_bound_histogram_f64_destroy,
    otel_bound_histogram_f64_record, otel_counter_u64_add, otel_counter_u64_bind,
    otel_counter_u64_destroy, otel_gauge_f64_destroy, otel_gauge_f64_record,
    otel_global_meter_provider, otel_histogram_f64_bind, otel_histogram_f64_destroy,
    otel_histogram_f64_record, otel_meter_create_f64_gauge, otel_meter_create_f64_histogram,
    otel_meter_create_u64_counter, otel_meter_destroy, otel_meter_provider_destroy,
    otel_meter_provider_get_meter, OtelAttributeType, OtelAttributeValue, OtelBoundCounterU64,
    OtelBoundHistogramF64, OtelCounterU64, OtelGaugeF64, OtelHistogramF64, OtelKeyValue,
    OtelStatus, OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_custom_metric_exporter_new, otel_manual_metric_reader_new, otel_sdk_build,
    otel_sdk_builder_add_manual_metric_reader, otel_sdk_builder_destroy, otel_sdk_builder_new,
    otel_sdk_destroy, otel_sdk_metrics_shutdown, otel_sdk_set_metrics_as_global,
    OtelCustomMetricExporterCallbacks, OtelManualMetricReader, OtelMetricBatch, OtelMetricExporter,
    OtelSdk,
};
use opentelemetry_sdk::metrics::{ManualReader, SdkMeterProvider};

const ITERATIONS: u64 = 10_000;
const WARMUP_ITERATIONS: u64 = 1_000;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
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

fn allocation_ceiling(name: &str) -> u64 {
    if name.starts_with("api_no_sdk/")
        || name.starts_with("rust_sdk/")
        || name.contains("/bound_counter_")
        || name.contains("/bound_histogram_")
    {
        return 0;
    }
    if let Some(rest) = name.strip_prefix("c_sdk/") {
        let count = rest
            .rsplit('/')
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .expect("benchmark name ends in an attribute count");
        return if rest.contains("/string/") {
            2 * count + 1
        } else {
            count + 1
        };
    }
    panic!("missing allocation contract for {name}");
}

fn measure(name: &str, mut operation: impl FnMut()) {
    for _ in 0..WARMUP_ITERATIONS {
        operation();
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::SeqCst);
    for _ in 0..ITERATIONS {
        operation();
    }
    COUNTING.store(false, Ordering::SeqCst);

    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    println!(
        "{name},{ITERATIONS},{allocations},{bytes},{:.6},{:.6}",
        allocations as f64 / ITERATIONS as f64,
        bytes as f64 / ITERATIONS as f64
    );
    let ceiling = allocation_ceiling(name) * ITERATIONS;
    assert!(
        allocations <= ceiling,
        "allocation contract exceeded for {name}: {allocations} total allocations, ceiling {ceiling}"
    );
}

struct CInstruments {
    provider: *mut opentelemetry_c_api::OtelMeterProvider,
    meter: *mut opentelemetry_c_api::OtelMeter,
    counter: *mut OtelCounterU64,
    gauge: *mut OtelGaugeF64,
    histogram: *mut OtelHistogramF64,
}

impl CInstruments {
    fn acquire() -> Self {
        let provider = otel_global_meter_provider();
        let meter =
            unsafe { otel_meter_provider_get_meter(provider, sv("bench"), empty(), empty()) };
        let mut counter = ptr::null_mut();
        let mut gauge = ptr::null_mut();
        let mut histogram = ptr::null_mut();
        assert_ok(unsafe {
            otel_meter_create_u64_counter(meter, sv("requests"), ptr::null(), &mut counter)
        });
        assert_ok(unsafe {
            otel_meter_create_f64_gauge(meter, sv("depth"), ptr::null(), &mut gauge)
        });
        assert_ok(unsafe {
            otel_meter_create_f64_histogram(meter, sv("duration"), ptr::null(), &mut histogram)
        });
        Self {
            provider,
            meter,
            counter,
            gauge,
            histogram,
        }
    }

    fn measure(&self, prefix: &str) {
        for shape in [
            AttributeShape::IntegerBool,
            AttributeShape::MixedNumeric,
            AttributeShape::String,
        ] {
            for count in [0, 1, 4, 8, 16] {
                let set = CAttributeSet::new(shape, count);
                let (attributes, attribute_count) = set.as_ffi();
                measure(
                    &format!("{prefix}/counter_u64/{}/{count}", shape.name()),
                    || {
                        black_box(unsafe {
                            otel_counter_u64_add(self.counter, 1, attributes, attribute_count)
                        });
                    },
                );
                measure(
                    &format!("{prefix}/gauge_f64/{}/{count}", shape.name()),
                    || {
                        black_box(unsafe {
                            otel_gauge_f64_record(self.gauge, 1.5, attributes, attribute_count)
                        });
                    },
                );
                measure(
                    &format!("{prefix}/histogram_f64/{}/{count}", shape.name()),
                    || {
                        black_box(unsafe {
                            otel_histogram_f64_record(
                                self.histogram,
                                1.5,
                                attributes,
                                attribute_count,
                            )
                        });
                    },
                );
                let mut bound_counter: *mut OtelBoundCounterU64 = ptr::null_mut();
                assert_ok(unsafe {
                    otel_counter_u64_bind(
                        self.counter,
                        attributes,
                        attribute_count,
                        &mut bound_counter,
                    )
                });
                let mut bound_histogram: *mut OtelBoundHistogramF64 = ptr::null_mut();
                assert_ok(unsafe {
                    otel_histogram_f64_bind(
                        self.histogram,
                        attributes,
                        attribute_count,
                        &mut bound_histogram,
                    )
                });
                measure(
                    &format!("{prefix}/bound_counter_u64/{}/{count}", shape.name()),
                    || {
                        black_box(unsafe { otel_bound_counter_u64_add(bound_counter, 1) });
                    },
                );
                measure(
                    &format!("{prefix}/bound_histogram_f64/{}/{count}", shape.name()),
                    || {
                        black_box(unsafe { otel_bound_histogram_f64_record(bound_histogram, 1.5) });
                    },
                );
                unsafe {
                    otel_bound_counter_u64_destroy(bound_counter);
                    otel_bound_histogram_f64_destroy(bound_histogram);
                }
            }
        }
    }
}

impl Drop for CInstruments {
    fn drop(&mut self) {
        unsafe {
            otel_histogram_f64_destroy(self.histogram);
            otel_gauge_f64_destroy(self.gauge);
            otel_counter_u64_destroy(self.counter);
            otel_meter_destroy(self.meter);
            otel_meter_provider_destroy(self.provider);
        }
    }
}

extern "C" fn export_metrics(_: *mut c_void, _: *const OtelMetricBatch) -> OtelStatus {
    OtelStatus::Ok
}

fn install_c_sdk() -> *mut OtelSdk {
    let callbacks = OtelCustomMetricExporterCallbacks {
        struct_size: std::mem::size_of::<OtelCustomMetricExporterCallbacks>(),
        export_metrics: Some(export_metrics),
        force_flush: None,
        shutdown: None,
        state_destroy: None,
    };
    let mut exporter: *mut OtelMetricExporter = ptr::null_mut();
    assert_ok(unsafe {
        otel_custom_metric_exporter_new(&callbacks, ptr::null_mut(), 1, &mut exporter)
    });
    let mut reader: *mut OtelManualMetricReader = ptr::null_mut();
    assert_ok(unsafe { otel_manual_metric_reader_new(exporter, &mut reader) });
    let builder = otel_sdk_builder_new();
    assert_ok(unsafe { otel_sdk_builder_add_manual_metric_reader(builder, reader) });
    let mut sdk = ptr::null_mut();
    assert_ok(unsafe { otel_sdk_build(builder, &mut sdk) });
    unsafe { otel_sdk_builder_destroy(builder) };
    assert_ok(unsafe { otel_sdk_set_metrics_as_global(sdk) });
    sdk
}

fn measure_direct_rust() {
    let provider = SdkMeterProvider::builder()
        .with_reader(ManualReader::builder().build())
        .build();
    let meter = provider.meter("bench-direct");
    let counter = meter.u64_counter("requests").build();
    let gauge = meter.f64_gauge("depth").build();
    let histogram = meter.f64_histogram("duration").build();

    for shape in [
        AttributeShape::IntegerBool,
        AttributeShape::MixedNumeric,
        AttributeShape::String,
    ] {
        for count in [0, 1, 4, 8, 16] {
            let attributes = rust_attributes(shape, count);
            measure(
                &format!("rust_sdk/counter_u64/{}/{count}", shape.name()),
                || counter.add(black_box(1), black_box(&attributes)),
            );
            measure(
                &format!("rust_sdk/gauge_f64/{}/{count}", shape.name()),
                || gauge.record(black_box(1.5), black_box(&attributes)),
            );
            measure(
                &format!("rust_sdk/histogram_f64/{}/{count}", shape.name()),
                || histogram.record(black_box(1.5), black_box(&attributes)),
            );
            let bound_counter = counter.bind(&attributes);
            measure(
                &format!("rust_sdk/bound_counter_u64/{}/{count}", shape.name()),
                || bound_counter.add(black_box(1)),
            );
            let bound_histogram = histogram.bind(&attributes);
            measure(
                &format!("rust_sdk/bound_histogram_f64/{}/{count}", shape.name()),
                || bound_histogram.record(black_box(1.5)),
            );
        }
    }

    let _ = provider.shutdown();
}

fn main() {
    println!(
        "benchmark,iterations,total_allocations,total_allocated_bytes,allocations_per_op,allocated_bytes_per_op"
    );

    CInstruments::acquire().measure("api_no_sdk");

    let sdk = install_c_sdk();
    CInstruments::acquire().measure("c_sdk");
    unsafe {
        let _ = otel_sdk_metrics_shutdown(sdk, 2_000);
        otel_sdk_destroy(sdk);
    }

    measure_direct_rust();
}
