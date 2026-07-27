//! API-only (no SDK installed) Logs behavior.
//!
//! Everything here runs in a process where no Logs implementation was ever registered, so it
//! exercises the no-op default path end to end.

use std::os::raw::c_char;

use opentelemetry_c_api as api;
use opentelemetry_c_api::{
    OtelLogRecordView, OtelLogTraceContext, OtelLogValue, OtelLogValuePayload, OtelLogValueType,
    OtelStatus, OtelStringView,
};

fn sv(s: &str) -> OtelStringView {
    OtelStringView {
        ptr: s.as_ptr().cast::<c_char>(),
        len: s.len(),
    }
}

fn empty_value() -> OtelLogValue {
    OtelLogValue {
        value_type: OtelLogValueType::Empty as u32,
        reserved: 0,
        value: OtelLogValuePayload {
            string_value: OtelStringView::empty(),
        },
    }
}

fn record() -> OtelLogRecordView {
    OtelLogRecordView {
        struct_size: std::mem::size_of::<OtelLogRecordView>() as u64,
        present_fields: 0,
        timestamp_unix_nanos: 0,
        observed_timestamp_unix_nanos: 0,
        severity_number: 0,
        reserved_flags: 0,
        body: empty_value(),
        attributes: std::ptr::null(),
        attribute_count: 0,
        value_nodes: std::ptr::null(),
        value_node_count: 0,
        trace_context: OtelLogTraceContext {
            trace_id: [0; 16],
            span_id: [0; 8],
            trace_flags: 0,
            reserved: [0; 7],
        },
        reserved: [0; 4],
    }
}

#[test]
fn provider_and_logger_are_acquirable_without_an_sdk() {
    let provider = api::otel_global_logger_provider();
    assert!(!provider.is_null());
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            sv("scope"),
            sv("1.0"),
            sv("https://example.test/schema"),
        )
    };
    assert!(!logger.is_null());
    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(provider) };
}

#[test]
fn enabled_is_false_without_an_sdk_and_for_unrepresentable_severities() {
    let provider = api::otel_global_logger_provider();
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };
    for severity in [0, 1, 9, 17, 24, 25, u32::MAX] {
        assert_eq!(unsafe { api::otel_logger_enabled(logger, severity) }, 0);
    }
    // A NULL logger is not enabled either.
    assert_eq!(unsafe { api::otel_logger_enabled(std::ptr::null(), 9) }, 0);
    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(provider) };
}

#[test]
fn emit_succeeds_without_inspecting_nested_content() {
    let provider = api::otel_global_logger_provider();
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };

    let mut view = record();
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &view) },
        OtelStatus::Ok
    );

    // Structurally invalid content that an SDK-backed logger would reject: a bogus body tag,
    // an out-of-range severity, unknown presence bits, and a dangling attribute pointer with
    // a non-zero count. The no-SDK path must not read any of it.
    view.body.value_type = 0xDEAD_BEEF;
    view.severity_number = 9_999;
    view.present_fields = u64::MAX;
    view.attributes = std::ptr::NonNull::dangling().as_ptr();
    view.attribute_count = 4;
    view.value_nodes = std::ptr::NonNull::dangling().as_ptr();
    view.value_node_count = 12;
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &view) },
        OtelStatus::Ok
    );

    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(provider) };
}

#[test]
fn null_and_malformed_arguments_are_rejected() {
    let provider = api::otel_global_logger_provider();
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };
    let view = record();

    assert_eq!(
        unsafe { api::otel_logger_emit(std::ptr::null(), &view) },
        OtelStatus::InvalidArgument
    );
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, std::ptr::null()) },
        OtelStatus::InvalidArgument
    );

    let mut undersized = record();
    undersized.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64 - 1;
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &undersized) },
        OtelStatus::InvalidArgument
    );

    // A larger struct_size from a newer caller stays acceptable (append-only evolution).
    let mut larger = record();
    larger.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64 + 64;
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &larger) },
        OtelStatus::Ok
    );

    // An empty instrumentation name is rejected.
    assert!(unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            OtelStringView::empty(),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    }
    .is_null());
    assert!(unsafe {
        api::otel_logger_provider_get_logger_with_options(provider, std::ptr::null())
    }
    .is_null());
    assert!(unsafe {
        api::otel_logger_provider_get_logger(
            std::ptr::null(),
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    }
    .is_null());

    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(provider) };
}

#[test]
fn destroy_is_a_noop_on_null_and_wrong_handle_types_are_rejected() {
    unsafe { api::otel_logger_destroy(std::ptr::null_mut()) };
    unsafe { api::otel_logger_provider_destroy(std::ptr::null_mut()) };

    // A live handle of a different project kind must be rejected before typed access.
    let meter_provider = api::otel_global_meter_provider();
    let view = record();
    assert_eq!(
        unsafe { api::otel_logger_emit(meter_provider.cast(), &view) },
        OtelStatus::InvalidArgument
    );
    assert_eq!(
        unsafe { api::otel_logger_enabled(meter_provider.cast(), 9) },
        0
    );
    assert!(unsafe {
        api::otel_logger_provider_get_logger(
            meter_provider.cast(),
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    }
    .is_null());
    unsafe { api::otel_meter_provider_destroy(meter_provider) };
}

#[test]
fn scope_attributes_are_validated_before_any_transfer() {
    use opentelemetry_c_api::{OtelAttributeType, OtelAttributeValue, OtelKeyValue};

    let provider = api::otel_global_logger_provider();
    let duplicate = [
        OtelKeyValue {
            key: sv("k"),
            value_type: OtelAttributeType::Int64 as u32,
            value: OtelAttributeValue { int64_value: 1 },
        },
        OtelKeyValue {
            key: sv("k"),
            value_type: OtelAttributeType::Int64 as u32,
            value: OtelAttributeValue { int64_value: 2 },
        },
    ];
    let options = api::OtelLoggerOptions {
        struct_size: std::mem::size_of::<api::OtelLoggerOptions>() as u64,
        name: sv("scope"),
        version: OtelStringView::empty(),
        schema_url: OtelStringView::empty(),
        attributes: duplicate.as_ptr(),
        attribute_count: duplicate.len(),
    };
    assert!(
        unsafe { api::otel_logger_provider_get_logger_with_options(provider, &options) }.is_null()
    );

    let mut undersized = options;
    undersized.struct_size = std::mem::size_of::<api::OtelLoggerOptions>() as u64 - 1;
    assert!(
        unsafe { api::otel_logger_provider_get_logger_with_options(provider, &undersized) }
            .is_null()
    );

    unsafe { api::otel_logger_provider_destroy(provider) };
}
