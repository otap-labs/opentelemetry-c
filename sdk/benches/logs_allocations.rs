//! Steady-state allocation counts for the experimental Logs emit path.
//!
//! The Criterion suite in `logs_hotpath` is the source for latency. This executable answers a
//! different and, for a logging bridge, more important question: **how many heap allocations
//! does one emitted record cost, and how much of that is the C boundary rather than the pinned
//! Rust SDK?** Latency on a loaded machine is noisy; allocation counts are deterministic, so a
//! regression here is unambiguous.
//!
//! Three paths are reported for the same payloads:
//!
//! * `noop` — public C API, no SDK installed. Establishes that a disabled Logs pipeline costs
//!   zero allocations, which is the property a C caller most needs to be able to rely on.
//! * `c_sdk` — public C API with a real SDK Logs pipeline. Every allocation here is one the
//!   bridge must make, because the borrowed record cannot outlive the call and therefore every
//!   string, byte string, and container must be copied into owned Rust storage.
//! * `rust` — the pinned Rust SDK driven directly with equivalent data, so the C figures have a
//!   reference point instead of being reported in a vacuum.
//!
//! The batch processor is configured with a small bounded queue and a one-hour scheduled delay.
//! The warm-up fills the queue, so every measured emit performs the full validate-convert-hand
//! off and is then dropped by the queue. That is deliberate: it keeps the numbers deterministic
//! and comparable. Letting the queue grow instead would fold amortized queue reallocation into
//! the per-record figures, and letting an export run would let background-thread allocations be
//! charged to whichever emit happened to be in flight, since the counting allocator is global.
//!
//! Output is CSV on stdout: `name,iterations,allocations,bytes,allocations_per_op,bytes_per_op`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::hint::black_box;
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{InstrumentationScope, Key};

use opentelemetry_c_abi::{
    OtelLogBytesView, OtelLogRecordView, OtelLogValue, OtelLogValueNode, OtelLogValuePayload,
    OtelLogValueRange, OtelLogValueType, OTEL_LOG_FIELD_TIMESTAMP, OTEL_LOG_FIELD_TRACE_CONTEXT,
};
use opentelemetry_c_api::{
    otel_global_logger_provider, otel_logger_destroy, otel_logger_emit,
    otel_logger_provider_destroy, otel_logger_provider_get_logger, OtelLogger, OtelLoggerProvider,
    OtelStatus, OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_batch_log_processor_builder_build, otel_batch_log_processor_builder_destroy,
    otel_batch_log_processor_builder_new, otel_batch_log_processor_builder_set_exporter,
    otel_batch_log_processor_builder_set_max_export_batch_size,
    otel_batch_log_processor_builder_set_max_queue_size,
    otel_batch_log_processor_builder_set_scheduled_delay_millis,
    otel_otlp_log_exporter_builder_build, otel_otlp_log_exporter_builder_destroy,
    otel_otlp_log_exporter_builder_new, otel_otlp_log_exporter_builder_set_endpoint,
    otel_sdk_build, otel_sdk_builder_add_log_processor, otel_sdk_builder_destroy,
    otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_logs_shutdown, otel_sdk_set_logs_as_global,
    OtelLogExporter, OtelLogProcessor, OtelSdk,
};

const ITERATIONS: u64 = 10_000;
const WARMUP_ITERATIONS: u64 = 1_000;
/// Small enough that the warm-up fills it, so the measured loop reaches a steady state in which
/// no queue growth or export is possible and only per-record work is counted.
const QUEUE_SIZE: usize = 512;
const DEAD_ENDPOINT: &str = "http://127.0.0.1:1/v1/logs";
const BODY_TEXT: &str = "request completed";
const NESTED_TEXT: &str = "nested-value";
/// 64 bytes: large enough that the copy is visible, small enough to stay a realistic payload.
const BODY_BYTES: &[u8; 64] = &[0x5A; 64];

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyShape {
    Absent,
    String,
    Bytes,
    Nested,
}

impl BodyShape {
    fn name(self) -> &'static str {
        match self {
            Self::Absent => "no_body",
            Self::String => "string_body",
            Self::Bytes => "bytes_body",
            Self::Nested => "nested_body",
        }
    }
}

/// Owns every buffer the borrowed record points at.
struct RecordFixture {
    _keys: Vec<String>,
    _values: Vec<String>,
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
    body: OtelLogValue,
}

impl RecordFixture {
    fn new(shape: BodyShape, attribute_count: usize) -> Self {
        let keys = (0..attribute_count)
            .map(|index| format!("benchmark.attribute.{index}"))
            .collect::<Vec<_>>();
        let values = (0..attribute_count)
            .map(|index| format!("benchmark-value-{index:02}"))
            .collect::<Vec<_>>();

        let attributes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| OtelLogValueNode {
                key: sv(key),
                value: match index % 3 {
                    0 => OtelLogValue {
                        value_type: OtelLogValueType::Int64 as u32,
                        reserved: 0,
                        value: OtelLogValuePayload {
                            int64_value: index as i64,
                        },
                    },
                    1 => OtelLogValue {
                        value_type: OtelLogValueType::String as u32,
                        reserved: 0,
                        value: OtelLogValuePayload {
                            string_value: sv(&values[index]),
                        },
                    },
                    _ => OtelLogValue {
                        value_type: OtelLogValueType::Double as u32,
                        reserved: 0,
                        value: OtelLogValuePayload {
                            double_value: index as f64 + 0.5,
                        },
                    },
                },
            })
            .collect::<Vec<_>>();

        let (nodes, body) = match shape {
            BodyShape::Absent => (
                Vec::new(),
                OtelLogValue {
                    value_type: OtelLogValueType::Empty as u32,
                    reserved: 0,
                    value: OtelLogValuePayload {
                        string_value: empty(),
                    },
                },
            ),
            BodyShape::String => (
                Vec::new(),
                OtelLogValue {
                    value_type: OtelLogValueType::String as u32,
                    reserved: 0,
                    value: OtelLogValuePayload {
                        string_value: sv(BODY_TEXT),
                    },
                },
            ),
            BodyShape::Bytes => (
                Vec::new(),
                OtelLogValue {
                    value_type: OtelLogValueType::Bytes as u32,
                    reserved: 0,
                    value: OtelLogValuePayload {
                        bytes_value: OtelLogBytesView {
                            ptr: BODY_BYTES.as_ptr(),
                            len: BODY_BYTES.len(),
                        },
                    },
                },
            ),
            // map { detail: [string, bool], count: int }, children always at greater indices.
            BodyShape::Nested => (
                vec![
                    OtelLogValueNode {
                        key: sv("detail"),
                        value: OtelLogValue {
                            value_type: OtelLogValueType::Array as u32,
                            reserved: 0,
                            value: OtelLogValuePayload {
                                children: OtelLogValueRange { first: 2, count: 2 },
                            },
                        },
                    },
                    OtelLogValueNode {
                        key: sv("count"),
                        value: OtelLogValue {
                            value_type: OtelLogValueType::Int64 as u32,
                            reserved: 0,
                            value: OtelLogValuePayload { int64_value: 7 },
                        },
                    },
                    OtelLogValueNode {
                        key: empty(),
                        value: OtelLogValue {
                            value_type: OtelLogValueType::String as u32,
                            reserved: 0,
                            value: OtelLogValuePayload {
                                string_value: sv(NESTED_TEXT),
                            },
                        },
                    },
                    OtelLogValueNode {
                        key: empty(),
                        value: OtelLogValue {
                            value_type: OtelLogValueType::Bool as u32,
                            reserved: 0,
                            value: OtelLogValuePayload { bool_value: 1 },
                        },
                    },
                ],
                OtelLogValue {
                    value_type: OtelLogValueType::Map as u32,
                    reserved: 0,
                    value: OtelLogValuePayload {
                        children: OtelLogValueRange { first: 0, count: 2 },
                    },
                },
            ),
        };

        Self {
            _keys: keys,
            _values: values,
            attributes,
            nodes,
            body,
        }
    }

    /// A trace-correlated variant of the same record. Correlation is measured separately
    /// because it is the one field the bridge must set explicitly on every emit to stop the
    /// ambient Rust `Context` from leaking into a C caller's record.
    fn correlated_record(&self) -> OtelLogRecordView {
        let mut record = self.record();
        record.present_fields |= OTEL_LOG_FIELD_TRACE_CONTEXT;
        record.trace_context.trace_id = [0x11; 16];
        record.trace_context.span_id = [0x22; 8];
        record.trace_context.trace_flags = 1;
        record
    }

    fn record(&self) -> OtelLogRecordView {
        let mut record: OtelLogRecordView = unsafe { std::mem::zeroed() };
        record.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64;
        record.present_fields = OTEL_LOG_FIELD_TIMESTAMP;
        record.timestamp_unix_nanos = 1_700_000_000_000_000_000;
        record.severity_number = Severity::Info as u32;
        record.body = self.body;
        if !self.attributes.is_empty() {
            record.attributes = self.attributes.as_ptr();
            record.attribute_count = self.attributes.len();
        }
        if !self.nodes.is_empty() {
            record.value_nodes = self.nodes.as_ptr();
            record.value_node_count = self.nodes.len();
        }
        record
    }
}

fn rust_attributes(count: usize) -> Vec<(Key, AnyValue)> {
    (0..count)
        .map(|index| {
            let key = Key::new(format!("benchmark.attribute.{index}"));
            let value = match index % 3 {
                0 => AnyValue::Int(index as i64),
                1 => AnyValue::String(format!("benchmark-value-{index:02}").into()),
                _ => AnyValue::Double(index as f64 + 0.5),
            };
            (key, value)
        })
        .collect()
}

fn rust_body(shape: BodyShape) -> Option<AnyValue> {
    match shape {
        BodyShape::Absent => None,
        BodyShape::String => Some(AnyValue::String(BODY_TEXT.into())),
        BodyShape::Bytes => Some(AnyValue::Bytes(Box::new(BODY_BYTES.to_vec()))),
        BodyShape::Nested => {
            let array = AnyValue::ListAny(Box::new(vec![
                AnyValue::String(NESTED_TEXT.into()),
                AnyValue::Boolean(true),
            ]));
            let mut map = HashMap::new();
            map.insert(Key::new("detail"), array);
            map.insert(Key::new("count"), AnyValue::Int(7));
            Some(AnyValue::Map(Box::new(map)))
        }
    }
}

fn allocation_ceiling(name: &str) -> Option<u64> {
    if name.starts_with("noop/") {
        return Some(0);
    }
    let rest = name.strip_prefix("c_sdk/")?;
    if rest == "trace_correlated/1" {
        return Some(12);
    }
    let (shape, count) = rest.rsplit_once('/')?;
    let count = count.parse::<usize>().ok()?;
    let index = ATTRIBUTE_COUNTS
        .iter()
        .position(|candidate| *candidate == count)?;
    // These ceilings intentionally leave modest headroom over the checked-in baseline while
    // still failing meaningful bridge regressions. Update them only with reviewed benchmark
    // evidence and an explanation in docs/PERFORMANCE.md.
    let ceilings = match shape {
        "no_body" => [3, 9, 17, 26, 31],
        "string_body" => [8, 13, 22, 30, 35],
        "bytes_body" => [10, 15, 24, 32, 36],
        "nested_body" => [32, 38, 48, 56, 61],
        _ => return None,
    };
    Some(ceilings[index])
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
    if let Some(per_operation) = allocation_ceiling(name) {
        let ceiling = per_operation * ITERATIONS;
        assert!(
            allocations <= ceiling,
            "allocation contract exceeded for {name}: {allocations} total allocations, ceiling {ceiling}"
        );
    }
}

struct CLogger {
    provider: *mut OtelLoggerProvider,
    logger: *mut OtelLogger,
}

impl CLogger {
    fn acquire() -> Self {
        let provider = otel_global_logger_provider();
        let logger =
            unsafe { otel_logger_provider_get_logger(provider, sv("bench"), empty(), empty()) };
        assert!(!logger.is_null(), "logger acquisition failed");
        Self { provider, logger }
    }
}

impl Drop for CLogger {
    fn drop(&mut self) {
        unsafe {
            otel_logger_destroy(self.logger);
            otel_logger_provider_destroy(self.provider);
        }
    }
}

fn install_sdk() -> *mut OtelSdk {
    unsafe {
        let exporter_builder = otel_otlp_log_exporter_builder_new();
        assert!(!exporter_builder.is_null());
        assert_ok(otel_otlp_log_exporter_builder_set_endpoint(
            exporter_builder,
            sv(DEAD_ENDPOINT),
        ));
        let mut exporter: *mut OtelLogExporter = ptr::null_mut();
        assert_ok(otel_otlp_log_exporter_builder_build(
            exporter_builder,
            &mut exporter,
        ));
        otel_otlp_log_exporter_builder_destroy(exporter_builder);

        let processor_builder = otel_batch_log_processor_builder_new();
        assert!(!processor_builder.is_null());
        assert_ok(otel_batch_log_processor_builder_set_exporter(
            processor_builder,
            exporter,
        ));
        assert_ok(otel_batch_log_processor_builder_set_max_queue_size(
            processor_builder,
            QUEUE_SIZE,
        ));
        assert_ok(otel_batch_log_processor_builder_set_max_export_batch_size(
            processor_builder,
            QUEUE_SIZE,
        ));
        assert_ok(otel_batch_log_processor_builder_set_scheduled_delay_millis(
            processor_builder,
            3_600_000,
        ));
        let mut processor: *mut OtelLogProcessor = ptr::null_mut();
        assert_ok(otel_batch_log_processor_builder_build(
            processor_builder,
            &mut processor,
        ));
        otel_batch_log_processor_builder_destroy(processor_builder);

        let builder = otel_sdk_builder_new();
        assert!(!builder.is_null());
        assert_ok(otel_sdk_builder_add_log_processor(builder, processor));
        let mut sdk: *mut OtelSdk = ptr::null_mut();
        assert_ok(otel_sdk_build(builder, &mut sdk));
        otel_sdk_builder_destroy(builder);
        assert_ok(otel_sdk_set_logs_as_global(sdk));
        sdk
    }
}

const SHAPES: [BodyShape; 4] = [
    BodyShape::Absent,
    BodyShape::String,
    BodyShape::Bytes,
    BodyShape::Nested,
];
const ATTRIBUTE_COUNTS: [usize; 5] = [0, 1, 3, 5, 6];

fn measure_c_paths(prefix: &str) {
    let logger = CLogger::acquire();
    for shape in SHAPES {
        for count in ATTRIBUTE_COUNTS {
            let fixture = RecordFixture::new(shape, count);
            let record = fixture.record();
            let name = format!("{prefix}/{}/{count}", shape.name());
            measure(&name, || {
                let status = unsafe { otel_logger_emit(logger.logger, &record) };
                black_box(status);
            });
        }
    }

    // Trace correlation is measured on its own because the bridge must set it explicitly on
    // every emit; folding it into the matrix would hide whether it costs an allocation.
    let fixture = RecordFixture::new(BodyShape::String, 1);
    let correlated = fixture.correlated_record();
    measure(&format!("{prefix}/trace_correlated/1"), || {
        let status = unsafe { otel_logger_emit(logger.logger, &correlated) };
        black_box(status);
    });
}

fn measure_rust_path() {
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::logs::{BatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider};

    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(DEAD_ENDPOINT)
        .build()
        .expect("OTLP log exporter must build without contacting the endpoint");
    let processor = BatchLogProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(QUEUE_SIZE)
                .with_max_export_batch_size(QUEUE_SIZE)
                .with_scheduled_delay(std::time::Duration::from_secs(3600))
                .build(),
        )
        .build();
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(processor)
        .build();
    let logger = provider.logger_with_scope(InstrumentationScope::builder("bench").build());

    for shape in SHAPES {
        for count in ATTRIBUTE_COUNTS {
            let attributes = rust_attributes(count);
            let body = rust_body(shape);
            let name = format!("rust/{}/{count}", shape.name());
            measure(&name, || {
                let mut record = logger.create_log_record();
                record.set_severity_number(Severity::Info);
                record.set_severity_text(Severity::Info.name());
                record.set_timestamp(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_nanos(1_700_000_000_000_000_000),
                );
                if let Some(body) = body.clone() {
                    record.set_body(body);
                }
                for (key, value) in &attributes {
                    record.add_attribute(key.clone(), value.clone());
                }
                logger.emit(record);
            });
        }
    }

    let _ = provider.shutdown_with_timeout(std::time::Duration::from_millis(0));
}

fn main() {
    println!("name,iterations,allocations,bytes,allocations_per_op,bytes_per_op");

    // No SDK installed yet: this must report zero allocations per operation.
    measure_c_paths("noop");

    let sdk = install_sdk();
    measure_c_paths("c_sdk");
    unsafe {
        otel_sdk_logs_shutdown(sdk, 0);
        otel_sdk_destroy(sdk);
    }

    measure_rust_path();
}
