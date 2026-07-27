//! Hot-path FFI-overhead benchmarks for the experimental Logs bridge.
//!
//! Requires the `otlp-http` cargo feature (part of default features); the `[[bench]]` target
//! sets `required-features`, so this is skipped in `--no-default-features` builds.
//!
//! Three paths are measured side by side for every shape so the cost can be attributed:
//!
//! * `noop` — the public C API with no SDK installed. This is the pure FFI and handle-check
//!   cost, and it is the floor a C caller pays even when Logs are disabled.
//! * `c_sdk` — the public C API with a real SDK Logs pipeline installed. The difference from
//!   `noop` is what the bridge itself costs: prefix validation, the two-pass value node pool
//!   walk, and the conversion into owned `AnyValue`s.
//! * `rust` — the pinned Rust SDK called directly with equivalent data. This is the baseline a
//!   Rust user would get, so the difference from `c_sdk` is the honest price of the C boundary
//!   rather than a number without a reference point.
//!
//! Attribute counts deliberately include 5 and 6. The pinned `SdkLogRecord` preallocates
//! capacity for 5 attributes inline, so 6 is the first count that spills to the heap; measuring
//! only round numbers would hide that step.
//!
//! No collector is required: the OTLP exporter targets a closed loopback port (`127.0.0.1:1`),
//! so background export attempts fail fast and are discarded. A batch processor with a very
//! large scheduled delay and a bounded queue keeps the measured loop free of timer-driven
//! flushes; `force_flush` is never called. This is **not** an exporter or network benchmark.
//!
//! Run with: `cargo bench -p opentelemetry-c-sdk --bench logs_hotpath`.

use std::hint::black_box;
use std::os::raw::c_char;
use std::ptr;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{InstrumentationScope, Key};

use opentelemetry_c_abi::{
    OtelLogRecordView, OtelLogValue, OtelLogValueNode, OtelLogValuePayload, OtelLogValueRange,
    OtelLogValueType, OTEL_LOG_FIELD_TIMESTAMP,
};
use opentelemetry_c_api::{
    otel_global_logger_provider, otel_logger_destroy, otel_logger_emit, otel_logger_enabled,
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
    otel_sdk_builder_new, otel_sdk_builder_set_service_name, otel_sdk_destroy,
    otel_sdk_set_logs_as_global, OtelLogExporter, OtelLogProcessor, OtelSdk,
};

/// Closed loopback port: connections are refused immediately, so export never blocks the loop.
const DEAD_ENDPOINT: &str = "http://127.0.0.1:1/v1/logs";
/// Attribute counts chosen to straddle the pinned record's inline attribute capacity of 5.
const ATTRIBUTE_COUNTS: [usize; 4] = [0, 1, 5, 6];

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

/// Owns every buffer a borrowed record points at, so the record itself stays trivially copyable
/// and the measured loop does no setup work.
struct RecordFixture {
    _keys: Vec<String>,
    _values: Vec<String>,
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
    body: OtelLogValue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyShape {
    /// No body at all: isolates attribute cost.
    Absent,
    /// A plain string body: the common logging case.
    String,
    /// A two-level map/array body: exercises the node pool walk and bottom-up conversion.
    Nested,
}

impl BodyShape {
    fn name(self) -> &'static str {
        match self {
            Self::Absent => "no_body",
            Self::String => "string_body",
            Self::Nested => "nested_body",
        }
    }
}

const BODY_TEXT: &str = "request completed";
const NESTED_TEXT: &str = "nested-value";

impl RecordFixture {
    fn new(shape: BodyShape, attribute_count: usize) -> Self {
        let keys = (0..attribute_count)
            .map(|index| format!("benchmark.attribute.{index}"))
            .collect::<Vec<_>>();
        let values = (0..attribute_count)
            .map(|index| format!("benchmark-value-{index:02}"))
            .collect::<Vec<_>>();

        // Alternate scalar kinds so the conversion switch is not trivially predicted.
        let attributes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let value = match index % 3 {
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
                };
                OtelLogValueNode {
                    key: sv(key),
                    value,
                }
            })
            .collect::<Vec<_>>();

        // Node pool for the nested shape:
        //   body  -> map with children [0, 2)
        //   [0]   "detail" -> array with children [2, 4)
        //   [1]   "count"  -> int
        //   [2]   array element: string
        //   [3]   array element: bool
        // Children always live at a strictly greater index than their parent, which is what
        // makes the pool acyclic by construction.
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
            BodyShape::Nested => {
                let nodes = vec![
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
                ];
                let body = OtelLogValue {
                    value_type: OtelLogValueType::Map as u32,
                    reserved: 0,
                    value: OtelLogValuePayload {
                        children: OtelLogValueRange { first: 0, count: 2 },
                    },
                };
                (nodes, body)
            }
        };

        Self {
            _keys: keys,
            _values: values,
            attributes,
            nodes,
            body,
        }
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

    /// The same payload expressed directly in pinned Rust types, for the baseline path.
    fn rust_attributes(&self, attribute_count: usize) -> Vec<(Key, AnyValue)> {
        (0..attribute_count)
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

    fn rust_body(&self, shape: BodyShape) -> Option<AnyValue> {
        match shape {
            BodyShape::Absent => None,
            BodyShape::String => Some(AnyValue::String(BODY_TEXT.into())),
            BodyShape::Nested => {
                let array = AnyValue::ListAny(Box::new(vec![
                    AnyValue::String(NESTED_TEXT.into()),
                    AnyValue::Boolean(true),
                ]));
                let mut map = std::collections::HashMap::new();
                map.insert(Key::new("detail"), array);
                map.insert(Key::new("count"), AnyValue::Int(7));
                Some(AnyValue::Map(Box::new(map)))
            }
        }
    }
}

/// A live SDK Logs pipeline installed in the API global slot, torn down on drop.
struct InstalledSdk {
    sdk: *mut OtelSdk,
}

impl InstalledSdk {
    fn new() -> Self {
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
                2048,
            ));
            assert_ok(otel_batch_log_processor_builder_set_max_export_batch_size(
                processor_builder,
                512,
            ));
            // Effectively disable timer-driven flushing for the duration of the run.
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
            assert_ok(otel_sdk_builder_set_service_name(builder, sv("logs-bench")));
            assert_ok(otel_sdk_builder_add_log_processor(builder, processor));
            let mut sdk: *mut OtelSdk = ptr::null_mut();
            assert_ok(otel_sdk_build(builder, &mut sdk));
            otel_sdk_builder_destroy(builder);
            assert_ok(otel_sdk_set_logs_as_global(sdk));
            Self { sdk }
        }
    }
}

impl Drop for InstalledSdk {
    fn drop(&mut self) {
        // Shut down without flushing: the endpoint is dead, so flushing would only add a
        // pointless connection-refused wait to the benchmark teardown.
        unsafe {
            opentelemetry_c_sdk::otel_sdk_logs_shutdown(self.sdk, 0);
            otel_sdk_destroy(self.sdk);
        }
    }
}

/// A logger handle plus its owning provider handle, released together.
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

fn bench_c_emit(criterion: &mut Criterion, group_name: &str) {
    let logger = CLogger::acquire();
    let mut group = criterion.benchmark_group(group_name);
    for shape in [BodyShape::Absent, BodyShape::String, BodyShape::Nested] {
        for count in ATTRIBUTE_COUNTS {
            let fixture = RecordFixture::new(shape, count);
            let record = fixture.record();
            group.bench_with_input(
                BenchmarkId::new(shape.name(), count),
                &count,
                |bencher, _| {
                    bencher.iter(|| {
                        let status = unsafe { otel_logger_emit(logger.logger, &record) };
                        black_box(status)
                    });
                },
            );
        }
    }
    group.finish();
}

fn logs_noop_emit(criterion: &mut Criterion) {
    // No SDK installed: the global slot is empty, so this is the pure API cost.
    bench_c_emit(criterion, "logs_noop_emit");

    let logger = CLogger::acquire();
    criterion.bench_function("logs_noop_enabled", |bencher| {
        bencher.iter(|| black_box(unsafe { otel_logger_enabled(logger.logger, 9) }));
    });
}

fn logs_sdk_emit(criterion: &mut Criterion) {
    let _sdk = InstalledSdk::new();
    bench_c_emit(criterion, "logs_sdk_emit");

    let logger = CLogger::acquire();
    criterion.bench_function("logs_sdk_enabled", |bencher| {
        bencher.iter(|| black_box(unsafe { otel_logger_enabled(logger.logger, 9) }));
    });
}

fn logs_rust_baseline(criterion: &mut Criterion) {
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
                .with_max_queue_size(2048)
                .with_max_export_batch_size(512)
                .with_scheduled_delay(std::time::Duration::from_secs(3600))
                .build(),
        )
        .build();
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(processor)
        .build();
    let logger = provider.logger_with_scope(InstrumentationScope::builder("bench").build());

    let mut group = criterion.benchmark_group("logs_rust_emit");
    for shape in [BodyShape::Absent, BodyShape::String, BodyShape::Nested] {
        for count in ATTRIBUTE_COUNTS {
            let fixture = RecordFixture::new(shape, count);
            let attributes = fixture.rust_attributes(count);
            let body = fixture.rust_body(shape);
            group.bench_with_input(
                BenchmarkId::new(shape.name(), count),
                &count,
                |bencher, _| {
                    bencher.iter(|| {
                        // The C path builds a fresh record per emit, so the baseline must too;
                        // reusing one record would compare different amounts of work.
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
                },
            );
        }
    }
    group.finish();

    let _ = provider.shutdown_with_timeout(std::time::Duration::from_millis(0));
}

criterion_group!(benches, logs_noop_emit, logs_sdk_emit, logs_rust_baseline);
criterion_main!(benches);
