// SPDX-License-Identifier: Apache-2.0

//! Conversion-cost benchmarks for the callback-based custom Logs exporter.
//!
//! `logs_hotpath` measures the *emit* direction: borrowed C memory converted into owned Rust.
//! This suite measures the opposite direction, which is the cost the custom exporter adds:
//! turning a finished `LogBatch` into the flattened, borrowed `otel_log_export_batch_view_t`
//! that the C callback receives.
//!
//! Three paths are measured for every record shape so the conversion cost can be attributed
//! rather than merely reported:
//!
//! * `queue_drop` — a batch processor whose bounded queue is full. The record is emitted and
//!   converted for the queue, then dropped; no export runs. This is the emit-side floor.
//! * `export_ignore` — a simple processor plus a custom exporter whose callback returns
//!   immediately. Compared with `queue_drop` this is an upper bound on export-view
//!   construction, since it also includes the simple processor's own bookkeeping.
//! * `export_traverse` — the same, with a callback that walks every attribute and every node
//!   of the flattened pool. This is what a realistic bridge actually pays.
//!
//! Batch-size scaling is measured separately through a batch processor: N records are emitted
//! and then flushed, so one measured iteration is one converted batch of N records.
//!
//! No collector and no network is involved: the exporter is the C callback itself.
//!
//! Run with: `cargo bench -p opentelemetry-c-sdk --bench logs_export_conversion`.

use std::hint::black_box;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use opentelemetry_c_abi::{
    OtelLogRecordView, OtelLogValue, OtelLogValueNode, OtelLogValuePayload, OtelLogValueRange,
    OtelLogValueType,
};
use opentelemetry_c_api::{
    otel_logger_destroy, otel_logger_emit, otel_logger_provider_destroy,
    otel_logger_provider_get_logger, OtelLogger, OtelLoggerProvider, OtelStatus, OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_batch_log_processor_builder_build, otel_batch_log_processor_builder_destroy,
    otel_batch_log_processor_builder_new, otel_batch_log_processor_builder_set_exporter,
    otel_batch_log_processor_builder_set_max_export_batch_size,
    otel_batch_log_processor_builder_set_max_queue_size,
    otel_batch_log_processor_builder_set_scheduled_delay_millis, otel_custom_log_exporter_new,
    otel_sdk_build, otel_sdk_builder_add_log_processor, otel_sdk_builder_destroy,
    otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_get_logger_provider,
    otel_sdk_logs_force_flush, otel_sdk_logs_shutdown, otel_simple_log_processor_create,
    OtelCustomLogExporterCallbacks, OtelLogExportBatchView, OtelLogExporter, OtelLogProcessor,
    OtelSdk,
};

const BODY_TEXT: &str = "request completed";
/// Attribute counts straddling the pinned record's inline attribute capacity of 5.
const ATTRIBUTE_COUNTS: [usize; 4] = [0, 1, 4, 8];
const BATCH_SIZES: [usize; 3] = [1, 64, 512];

static VISITED: AtomicUsize = AtomicUsize::new(0);

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

extern "C" fn export_ignore(
    _data: *mut c_void,
    _batch: *const OtelLogExportBatchView,
) -> OtelStatus {
    OtelStatus::Ok
}

/// Walk everything a real bridge would walk, so the cost is not optimized away.
extern "C" fn export_traverse(
    _data: *mut c_void,
    batch: *const OtelLogExportBatchView,
) -> OtelStatus {
    let batch = unsafe { &*batch };
    let mut visited = 0_usize;
    if batch.record_count > 0 {
        let records = unsafe { std::slice::from_raw_parts(batch.records, batch.record_count) };
        for record in records {
            visited += 1;
            if record.attribute_count > 0 {
                let attributes = unsafe {
                    std::slice::from_raw_parts(record.attributes, record.attribute_count)
                };
                for attribute in attributes {
                    visited += usize::from(attribute.key.len != 0);
                    visited += attribute.value.value_type as usize;
                }
            }
            if record.value_node_count > 0 {
                let nodes = unsafe {
                    std::slice::from_raw_parts(record.value_nodes, record.value_node_count)
                };
                for node in nodes {
                    visited += node.value.value_type as usize;
                }
            }
        }
    }
    VISITED.fetch_add(visited, Ordering::Relaxed);
    OtelStatus::Ok
}

/// One built pipeline plus the handles that must be released in order.
struct Pipeline {
    sdk: *mut OtelSdk,
    provider: *mut OtelLoggerProvider,
    logger: *mut OtelLogger,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        unsafe {
            otel_logger_destroy(self.logger);
            otel_logger_provider_destroy(self.provider);
            let _ = otel_sdk_logs_shutdown(self.sdk, 1_000);
            otel_sdk_destroy(self.sdk);
        }
    }
}

fn custom_exporter(
    callback: extern "C" fn(*mut c_void, *const OtelLogExportBatchView) -> OtelStatus,
) -> *mut OtelLogExporter {
    let callbacks = OtelCustomLogExporterCallbacks {
        struct_size: std::mem::size_of::<OtelCustomLogExporterCallbacks>(),
        export_logs: Some(callback),
        shutdown: None,
        state_destroy: None,
    };
    let mut exporter: *mut OtelLogExporter = ptr::null_mut();
    let status =
        unsafe { otel_custom_log_exporter_new(&callbacks, ptr::null_mut(), &mut exporter) };
    assert_eq!(status, OtelStatus::Ok, "custom exporter construction");
    exporter
}

/// `batched` selects the batch processor; `queue_size` bounds it so a full queue drops records.
fn build_pipeline(processor: *mut OtelLogProcessor) -> Pipeline {
    let builder = otel_sdk_builder_new();
    assert_eq!(
        unsafe { otel_sdk_builder_add_log_processor(builder, processor) },
        OtelStatus::Ok
    );
    let mut sdk: *mut OtelSdk = ptr::null_mut();
    assert_eq!(unsafe { otel_sdk_build(builder, &mut sdk) }, OtelStatus::Ok);
    unsafe { otel_sdk_builder_destroy(builder) };
    let provider = unsafe { otel_sdk_get_logger_provider(sdk) }.cast::<OtelLoggerProvider>();
    let logger = unsafe { otel_logger_provider_get_logger(provider, sv("bench"), sv(""), sv("")) };
    assert!(!logger.is_null());
    Pipeline {
        sdk,
        provider,
        logger,
    }
}

fn simple_pipeline(
    callback: extern "C" fn(*mut c_void, *const OtelLogExportBatchView) -> OtelStatus,
) -> Pipeline {
    let exporter = custom_exporter(callback);
    let mut processor: *mut OtelLogProcessor = ptr::null_mut();
    assert_eq!(
        unsafe { otel_simple_log_processor_create(exporter, &mut processor) },
        OtelStatus::Ok
    );
    build_pipeline(processor)
}

fn batch_pipeline(
    callback: extern "C" fn(*mut c_void, *const OtelLogExportBatchView) -> OtelStatus,
    queue_size: usize,
    export_batch_size: usize,
    delay_millis: u64,
) -> Pipeline {
    let exporter = custom_exporter(callback);
    let builder = otel_batch_log_processor_builder_new();
    unsafe {
        assert_eq!(
            otel_batch_log_processor_builder_set_exporter(builder, exporter),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_batch_log_processor_builder_set_max_queue_size(builder, queue_size),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_batch_log_processor_builder_set_max_export_batch_size(builder, export_batch_size),
            OtelStatus::Ok
        );
        assert_eq!(
            otel_batch_log_processor_builder_set_scheduled_delay_millis(builder, delay_millis),
            OtelStatus::Ok
        );
    }
    let mut processor: *mut OtelLogProcessor = ptr::null_mut();
    assert_eq!(
        unsafe { otel_batch_log_processor_builder_build(builder, &mut processor) },
        OtelStatus::Ok
    );
    unsafe { otel_batch_log_processor_builder_destroy(builder) };
    build_pipeline(processor)
}

/// Borrowed record storage; every pointer refers to this struct or to `'static` data.
struct RecordData {
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
}

fn scalar(value_type: OtelLogValueType, value: OtelLogValuePayload) -> OtelLogValue {
    OtelLogValue {
        value_type: value_type as u32,
        reserved: 0,
        value,
    }
}

fn record_data(attribute_count: usize, structured: bool) -> RecordData {
    const KEYS: [&str; 8] = ["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7"];
    let mut attributes = Vec::new();
    let mut nodes = Vec::new();
    for key in KEYS.iter().take(attribute_count) {
        attributes.push(OtelLogValueNode {
            key: sv(key),
            value: scalar(
                OtelLogValueType::Int64,
                OtelLogValuePayload { int64_value: 7 },
            ),
        });
    }
    if structured {
        // { "list": [1, 2, 3] } — one map entry addressing three array elements.
        nodes.push(OtelLogValueNode {
            key: sv("list"),
            value: scalar(
                OtelLogValueType::Array,
                OtelLogValuePayload {
                    children: OtelLogValueRange { first: 1, count: 3 },
                },
            ),
        });
        for value in 1..=3 {
            nodes.push(OtelLogValueNode {
                key: sv(""),
                value: scalar(
                    OtelLogValueType::Int64,
                    OtelLogValuePayload { int64_value: value },
                ),
            });
        }
        attributes.push(OtelLogValueNode {
            key: sv("structured"),
            value: scalar(
                OtelLogValueType::Map,
                OtelLogValuePayload {
                    children: OtelLogValueRange { first: 0, count: 1 },
                },
            ),
        });
    }
    RecordData { attributes, nodes }
}

fn record_view(data: &RecordData, with_body: bool) -> OtelLogRecordView {
    OtelLogRecordView {
        struct_size: std::mem::size_of::<OtelLogRecordView>() as u64,
        present_fields: 0,
        timestamp_unix_nanos: 0,
        observed_timestamp_unix_nanos: 0,
        severity_number: 9,
        reserved_flags: 0,
        body: if with_body {
            scalar(
                OtelLogValueType::String,
                OtelLogValuePayload {
                    string_value: sv(BODY_TEXT),
                },
            )
        } else {
            scalar(
                OtelLogValueType::Empty,
                OtelLogValuePayload {
                    string_value: sv(""),
                },
            )
        },
        attributes: if data.attributes.is_empty() {
            ptr::null()
        } else {
            data.attributes.as_ptr()
        },
        attribute_count: data.attributes.len(),
        value_nodes: if data.nodes.is_empty() {
            ptr::null()
        } else {
            data.nodes.as_ptr()
        },
        value_node_count: data.nodes.len(),
        trace_context: unsafe { std::mem::zeroed() },
        reserved: [0; 4],
    }
}

fn bench_shapes(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("logs_export_conversion");

    for attribute_count in ATTRIBUTE_COUNTS {
        let data = record_data(attribute_count, false);
        let view = record_view(&data, true);

        // Full queue: emitted records are converted for the queue and then dropped, so no
        // export view is ever built. This is the emit-side floor for the same payload.
        let queue_drop = batch_pipeline(export_ignore, 2, 2, 3_600_000);
        for _ in 0..64 {
            let _ = unsafe { otel_logger_emit(queue_drop.logger, &view) };
        }
        group.bench_with_input(
            BenchmarkId::new("queue_drop", attribute_count),
            &attribute_count,
            |bencher, _| {
                bencher.iter(|| black_box(unsafe { otel_logger_emit(queue_drop.logger, &view) }))
            },
        );
        drop(queue_drop);

        let ignoring = simple_pipeline(export_ignore);
        group.bench_with_input(
            BenchmarkId::new("export_ignore", attribute_count),
            &attribute_count,
            |bencher, _| {
                bencher.iter(|| black_box(unsafe { otel_logger_emit(ignoring.logger, &view) }))
            },
        );
        drop(ignoring);

        let traversing = simple_pipeline(export_traverse);
        group.bench_with_input(
            BenchmarkId::new("export_traverse", attribute_count),
            &attribute_count,
            |bencher, _| {
                bencher.iter(|| black_box(unsafe { otel_logger_emit(traversing.logger, &view) }))
            },
        );
        drop(traversing);
    }

    // A structured body/attribute exercises the flattening queue rather than the scalar path.
    let structured = record_data(1, true);
    let structured_view = record_view(&structured, true);
    let traversing = simple_pipeline(export_traverse);
    group.bench_function("export_traverse/structured", |bencher| {
        bencher.iter(|| black_box(unsafe { otel_logger_emit(traversing.logger, &structured_view) }))
    });
    drop(traversing);

    group.finish();
}

fn bench_batch_sizes(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("logs_export_batch");
    let data = record_data(4, false);
    let view = record_view(&data, true);

    for size in BATCH_SIZES {
        // A generous queue and a long delay keep the timer from flushing mid-iteration, so one
        // measured iteration is exactly one emit burst plus one flush of `size` records.
        let pipeline = batch_pipeline(export_traverse, size * 4, size, 3_600_000);
        group.bench_with_input(
            BenchmarkId::new("emit_and_flush", size),
            &size,
            |bencher, _| {
                bencher.iter(|| {
                    for _ in 0..size {
                        let _ = unsafe { otel_logger_emit(pipeline.logger, &view) };
                    }
                    black_box(unsafe { otel_sdk_logs_force_flush(pipeline.sdk) })
                })
            },
        );
        drop(pipeline);
    }
    group.finish();
}

criterion_group!(benches, bench_shapes, bench_batch_sizes);
criterion_main!(benches);
