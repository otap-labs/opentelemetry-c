#![no_main]

//! Fuzzes the custom Logs exporter callback ABI end to end.
//!
//! Two things are under test. First, `otel_custom_log_exporter_new` must reject every
//! malformed callback table without taking ownership of the callback state. Second, whatever
//! the SDK hands back to the export callback must satisfy the published view invariants: the
//! declared struct sizes, only known presence bits, in-bounds child ranges, strictly forward
//! child indices, and every pool node referenced exactly once.
//!
//! No fuzzer-supplied address is ever dereferenced: only sizes, tags, counts and value kinds
//! are fuzzed, and every pointer either is NULL or points at a live buffer.

use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{
    OtelLogValue, OtelLogValueNode, OtelLogValuePayload, OtelLogValueRange, OtelLogValueType,
    OTEL_LOG_FIELD_OBSERVED_TIMESTAMP, OTEL_LOG_FIELD_TIMESTAMP,
};
use opentelemetry_c_api::{
    otel_logger_destroy, otel_logger_emit, otel_logger_provider_destroy,
    otel_logger_provider_get_logger, OtelLogRecordView, OtelLoggerProvider, OtelStatus,
    OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_custom_log_exporter_new, otel_log_exporter_destroy, otel_sdk_build,
    otel_sdk_builder_add_log_processor, otel_sdk_builder_destroy, otel_sdk_builder_new,
    otel_sdk_destroy, otel_sdk_get_logger_provider, otel_sdk_logs_force_flush,
    otel_sdk_logs_shutdown, otel_simple_log_processor_create, OtelCustomLogExporterCallbacks,
    OtelLogExportBatchView, OtelLogExportRecordView, OtelLogExporter, OtelLogProcessor, OtelSdk,
    OTEL_LOG_EXPORT_FIELD_KNOWN_MASK, OTEL_LOG_EXPORT_MAX_RECORDS,
};

#[derive(Arbitrary, Debug)]
struct Input {
    callback_size: usize,
    include_export: bool,
    include_shutdown: bool,
    include_destroy: bool,
    callback_status: u32,
    records: Vec<Record>,
}

#[derive(Arbitrary, Debug)]
struct Record {
    severity: u32,
    present_fields: u64,
    timestamp: u64,
    body_kind: u8,
    attribute_count: u8,
    nested: bool,
}

struct State {
    callback_status: u32,
    exports: AtomicUsize,
    destroys: AtomicUsize,
}

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

/// Validate one record view against every invariant the public header promises.
fn check_record(record: &OtelLogExportRecordView) {
    assert_eq!(
        record.struct_size as usize,
        std::mem::size_of::<OtelLogExportRecordView>()
    );
    assert_eq!(record.present_fields & !OTEL_LOG_EXPORT_FIELD_KNOWN_MASK, 0);
    assert_eq!(record.reserved_flags, 0);
    assert_eq!(record.reserved, [0; 4]);
    assert!(!record.scope.is_null());

    let nodes = if record.value_node_count == 0 {
        &[][..]
    } else {
        assert!(!record.value_nodes.is_null());
        unsafe { std::slice::from_raw_parts(record.value_nodes, record.value_node_count) }
    };
    let attributes = if record.attribute_count == 0 {
        &[][..]
    } else {
        assert!(!record.attributes.is_null());
        unsafe { std::slice::from_raw_parts(record.attributes, record.attribute_count) }
    };

    // Every node must be reachable from exactly one parent, and containers may only address
    // children that live at a strictly greater index.
    let mut references = vec![0_usize; nodes.len()];
    let mut visit = |value: &OtelLogValue, owner: Option<usize>| {
        if value.value_type != OtelLogValueType::Array as u32
            && value.value_type != OtelLogValueType::Map as u32
        {
            return;
        }
        let range: OtelLogValueRange = unsafe { value.value.children };
        let first = range.first as usize;
        let count = range.count as usize;
        assert!(first + count <= nodes.len());
        if let Some(owner) = owner {
            assert!(first > owner);
        }
        for slot in references.iter_mut().skip(first).take(count) {
            *slot += 1;
        }
    };
    visit(&record.body, None);
    for attribute in attributes {
        visit(&attribute.value, None);
    }
    for (index, node) in nodes.iter().enumerate() {
        visit(&node.value, Some(index));
    }
    for count in references {
        assert_eq!(count, 1, "every pool node is referenced exactly once");
    }
}

extern "C" fn export_logs(data: *mut c_void, batch: *const OtelLogExportBatchView) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
    state.exports.fetch_add(1, Ordering::Relaxed);
    let batch = unsafe { &*batch };
    assert_eq!(
        batch.struct_size as usize,
        std::mem::size_of::<OtelLogExportBatchView>()
    );
    assert!(batch.record_count <= OTEL_LOG_EXPORT_MAX_RECORDS);
    assert_eq!(batch.reserved, [0; 4]);
    if batch.record_count > 0 {
        let records = unsafe { std::slice::from_raw_parts(batch.records, batch.record_count) };
        for record in records {
            check_record(record);
        }
    }
    OtelStatus(state.callback_status)
}

extern "C" fn shutdown_state(data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
    OtelStatus(state.callback_status)
}

extern "C" fn destroy_state(data: *mut c_void) {
    let state = unsafe { &*(data.cast::<State>()) };
    state.destroys.fetch_add(1, Ordering::Relaxed);
}

fn prefix_size(raw: usize, complete: usize) -> usize {
    match raw % 5 {
        0 => complete,
        1 => 0,
        2 => complete.saturating_sub(1),
        3 => complete.saturating_add(1),
        _ => raw,
    }
}

fn empty_value() -> OtelLogValue {
    OtelLogValue {
        value_type: OtelLogValueType::Empty as u32,
        reserved: 0,
        value: OtelLogValuePayload {
            string_value: OtelStringView {
                ptr: ptr::null(),
                len: 0,
            },
        },
    }
}

fn emit(logger: *mut opentelemetry_c_api::OtelLogger, spec: &Record) {
    // Attribute values live in this frame and are borrowed only for the emit call.
    let attribute_count = usize::from(spec.attribute_count % 4);
    let mut nodes: Vec<OtelLogValueNode> = Vec::new();
    let mut attributes: Vec<OtelLogValueNode> = Vec::new();
    for index in 0..attribute_count {
        attributes.push(OtelLogValueNode {
            key: sv(["a", "b", "c", "d"][index % 4]),
            value: OtelLogValue {
                value_type: OtelLogValueType::Int64 as u32,
                reserved: 0,
                value: OtelLogValuePayload {
                    int64_value: index as i64,
                },
            },
        });
    }
    if spec.nested {
        // { "m": [0] }: one map entry addressing one array element, both strictly forward.
        nodes.push(OtelLogValueNode {
            key: sv("m"),
            value: OtelLogValue {
                value_type: OtelLogValueType::Array as u32,
                reserved: 0,
                value: OtelLogValuePayload {
                    children: OtelLogValueRange { first: 1, count: 1 },
                },
            },
        });
        nodes.push(OtelLogValueNode {
            key: sv(""),
            value: OtelLogValue {
                value_type: OtelLogValueType::Int64 as u32,
                reserved: 0,
                value: OtelLogValuePayload { int64_value: 0 },
            },
        });
        attributes.push(OtelLogValueNode {
            key: sv("nested"),
            value: OtelLogValue {
                value_type: OtelLogValueType::Map as u32,
                reserved: 0,
                value: OtelLogValuePayload {
                    children: OtelLogValueRange { first: 0, count: 1 },
                },
            },
        });
    }

    let body = match spec.body_kind % 5 {
        0 => empty_value(),
        1 => OtelLogValue {
            value_type: OtelLogValueType::String as u32,
            reserved: 0,
            value: OtelLogValuePayload {
                string_value: sv("body"),
            },
        },
        2 => OtelLogValue {
            value_type: OtelLogValueType::Int64 as u32,
            reserved: 0,
            value: OtelLogValuePayload { int64_value: -1 },
        },
        3 => OtelLogValue {
            value_type: OtelLogValueType::Double as u32,
            reserved: 0,
            value: OtelLogValuePayload { double_value: 0.5 },
        },
        _ => OtelLogValue {
            value_type: OtelLogValueType::Bool as u32,
            reserved: 0,
            value: OtelLogValuePayload { bool_value: 1 },
        },
    };

    let mut record = OtelLogRecordView {
        struct_size: std::mem::size_of::<OtelLogRecordView>() as u64,
        present_fields: spec.present_fields
            & (OTEL_LOG_FIELD_TIMESTAMP | OTEL_LOG_FIELD_OBSERVED_TIMESTAMP),
        timestamp_unix_nanos: spec.timestamp,
        observed_timestamp_unix_nanos: spec.timestamp,
        severity_number: spec.severity,
        reserved_flags: 0,
        body,
        attributes: ptr::null(),
        attribute_count: 0,
        value_nodes: ptr::null(),
        value_node_count: 0,
        trace_context: unsafe { std::mem::zeroed() },
        reserved: [0; 4],
    };
    if !attributes.is_empty() {
        record.attributes = attributes.as_ptr();
        record.attribute_count = attributes.len();
    }
    if !nodes.is_empty() {
        record.value_nodes = nodes.as_ptr();
        record.value_node_count = nodes.len();
    }
    let _ = unsafe { otel_logger_emit(logger, &record) };
}

fuzz_target!(|input: Input| {
    let state = State {
        callback_status: match input.callback_status % 10 {
            0..=7 => input.callback_status % 10,
            _ => input.callback_status,
        },
        exports: AtomicUsize::new(0),
        destroys: AtomicUsize::new(0),
    };
    let callbacks = OtelCustomLogExporterCallbacks {
        struct_size: prefix_size(
            input.callback_size,
            std::mem::size_of::<OtelCustomLogExporterCallbacks>(),
        ),
        export_logs: input.include_export.then_some(export_logs),
        shutdown: input.include_shutdown.then_some(shutdown_state),
        state_destroy: input.include_destroy.then_some(destroy_state),
    };
    let mut exporter: *mut OtelLogExporter = ptr::null_mut();
    let status = unsafe {
        otel_custom_log_exporter_new(
            &callbacks,
            (&state as *const State).cast_mut().cast(),
            &mut exporter,
        )
    };
    if status != OtelStatus::Ok {
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::Relaxed), 0);
        return;
    }

    let mut processor: *mut OtelLogProcessor = ptr::null_mut();
    if unsafe { otel_simple_log_processor_create(exporter, &mut processor) } != OtelStatus::Ok {
        unsafe { otel_log_exporter_destroy(exporter) };
        return;
    }
    let builder = otel_sdk_builder_new();
    if unsafe { otel_sdk_builder_add_log_processor(builder, processor) } != OtelStatus::Ok {
        unsafe { otel_sdk_builder_destroy(builder) };
        return;
    }
    let mut sdk: *mut OtelSdk = ptr::null_mut();
    if unsafe { otel_sdk_build(builder, &mut sdk) } != OtelStatus::Ok {
        unsafe { otel_sdk_builder_destroy(builder) };
        return;
    }
    unsafe { otel_sdk_builder_destroy(builder) };

    let provider = unsafe { otel_sdk_get_logger_provider(sdk) }.cast::<OtelLoggerProvider>();
    let logger =
        unsafe { otel_logger_provider_get_logger(provider, sv("fuzz"), sv("1.0"), sv("")) };
    if !logger.is_null() {
        for spec in input.records.iter().take(16) {
            emit(logger, spec);
        }
        let _ = unsafe { otel_sdk_logs_force_flush(sdk) };
    }

    unsafe {
        otel_logger_destroy(logger);
        otel_logger_provider_destroy(provider);
        let _ = otel_sdk_logs_shutdown(sdk, 1_000);
        otel_sdk_destroy(sdk);
    }
    let state_destroy_end =
        std::mem::offset_of!(OtelCustomLogExporterCallbacks, state_destroy)
            + std::mem::size_of_val(&callbacks.state_destroy);
    let expected_destroys =
        usize::from(input.include_destroy && callbacks.struct_size >= state_destroy_end);
    assert_eq!(
        state.destroys.load(Ordering::Relaxed),
        expected_destroys
    );
});
