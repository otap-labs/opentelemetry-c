#![no_main]
// SPDX-License-Identifier: Apache-2.0

//! Fuzzes the custom Traces exporter callback ABI end to end.
//!
//! Two things are under test. First, `otel_custom_trace_exporter_new` must reject every
//! malformed callback table without taking ownership of the callback state. Second, whatever
//! the SDK hands to the export callback must satisfy the published view invariants: the
//! declared struct sizes, zeroed reserved fields, record/event/link counts within their
//! bounds, a non-NULL scope, and attribute tags that are either a known scalar kind or a
//! one-level homogeneous array whose backing pointer is live whenever it is non-empty.
//!
//! No fuzzer-supplied address is ever dereferenced: only sizes, tags, counts, and value kinds
//! are fuzzed, and every pointer either is NULL or points at a live buffer.

use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{OtelKeyValue, OtelStatus, OtelStringView};
use opentelemetry_c_api::{
    otel_span_add_event, otel_span_destroy, otel_span_end, otel_span_set_int64_attribute,
    otel_span_set_string_attribute, otel_tracer_destroy, otel_tracer_provider_destroy,
    otel_tracer_provider_get_tracer, otel_tracer_start_span, OtelSpan, OtelSpanStartOptions,
    OtelTracer, OtelTracerProvider,
};
use opentelemetry_c_sdk::{
    otel_custom_trace_exporter_new, otel_sdk_build, otel_sdk_builder_add_span_processor,
    otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_get_tracer_provider,
    otel_sdk_shutdown, otel_simple_span_processor_create, otel_trace_exporter_destroy,
    OtelCustomTraceExporterCallbacks, OtelSdk, OtelSpanAttribute, OtelSpanEventView,
    OtelSpanExportBatchView, OtelSpanExportLinkView, OtelSpanExportRecordView, OtelSpanProcessor,
    OtelTraceExporter, OTEL_SPAN_ATTRIBUTE_DOUBLE_ARRAY, OTEL_SPAN_EXPORT_MAX_SPANS,
};

#[derive(Arbitrary, Debug)]
struct Input {
    callback_size: usize,
    include_export: bool,
    include_force_flush: bool,
    include_shutdown: bool,
    include_destroy: bool,
    callback_status: u32,
    spans: Vec<SpanSpec>,
}

#[derive(Arbitrary, Debug)]
struct SpanSpec {
    kind: u32,
    attribute_count: u8,
    add_event: bool,
    event_attribute_count: u8,
    name_index: u8,
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

/// Validate one span attribute: the tag is known, and an array tag names a live buffer.
fn check_attribute(attr: &OtelSpanAttribute) {
    assert!(attr.value_type <= OTEL_SPAN_ATTRIBUTE_DOUBLE_ARRAY);
    if attr.value_type >= 4 {
        // Array tag: the union's `array` member is active.
        let array = unsafe { attr.value.array };
        if array.count > 0 {
            assert!(!array.values.is_null());
        }
    }
}

fn check_attributes(ptr: *const OtelSpanAttribute, count: usize) {
    if count == 0 {
        return;
    }
    assert!(!ptr.is_null());
    let attributes = unsafe { std::slice::from_raw_parts(ptr, count) };
    for attribute in attributes {
        check_attribute(attribute);
    }
}

fn check_event(event: &OtelSpanEventView) {
    assert_eq!(
        event.struct_size as usize,
        std::mem::size_of::<OtelSpanEventView>()
    );
    assert_eq!(event.reserved_flags, 0);
    assert_eq!(event.reserved, [0; 2]);
    check_attributes(event.attributes, event.attribute_count);
}

fn check_link(link: &OtelSpanExportLinkView) {
    assert_eq!(
        link.struct_size as usize,
        std::mem::size_of::<OtelSpanExportLinkView>()
    );
    assert_eq!(link.reserved_flags, 0);
    assert_eq!(link.reserved_padding, [0; 3]);
    assert_eq!(link.reserved, [0; 2]);
    check_attributes(link.attributes, link.attribute_count);
}

/// Validate one record view against every invariant the public header promises.
fn check_record(record: &OtelSpanExportRecordView) {
    assert_eq!(
        record.struct_size as usize,
        std::mem::size_of::<OtelSpanExportRecordView>()
    );
    assert_eq!(record.reserved_flags, 0);
    assert_eq!(record.reserved_padding, [0; 3]);
    assert_eq!(record.reserved, [0; 4]);

    assert!(!record.scope.is_null());
    let scope = unsafe { &*record.scope };
    assert_eq!(
        scope.struct_size as usize,
        std::mem::size_of::<opentelemetry_c_sdk::OtelSpanExportScopeView>()
    );
    check_attributes(scope.attributes, scope.attribute_count);

    check_attributes(record.attributes, record.attribute_count);

    if record.event_count > 0 {
        assert!(!record.events.is_null());
        let events = unsafe { std::slice::from_raw_parts(record.events, record.event_count) };
        for event in events {
            check_event(event);
        }
    }
    if record.link_count > 0 {
        assert!(!record.links.is_null());
        let links = unsafe { std::slice::from_raw_parts(record.links, record.link_count) };
        for link in links {
            check_link(link);
        }
    }
}

extern "C" fn export_spans(data: *mut c_void, batch: *const OtelSpanExportBatchView) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
    state.exports.fetch_add(1, Ordering::Relaxed);
    let batch = unsafe { &*batch };
    assert_eq!(
        batch.struct_size as usize,
        std::mem::size_of::<OtelSpanExportBatchView>()
    );
    assert!(batch.record_count <= OTEL_SPAN_EXPORT_MAX_SPANS);
    assert_eq!(batch.reserved, [0; 4]);
    check_attributes(batch.resource_attributes, batch.resource_attribute_count);
    if batch.record_count > 0 {
        let records = unsafe { std::slice::from_raw_parts(batch.records, batch.record_count) };
        for record in records {
            check_record(record);
        }
    }
    OtelStatus(state.callback_status)
}

extern "C" fn force_flush_state(data: *mut c_void) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
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

fn emit(tracer: *mut OtelTracer, spec: &SpanSpec) {
    let names = ["a", "handle", "query", "work"];
    let options = OtelSpanStartOptions {
        kind: spec.kind,
        parent: ptr::null(),
    };
    let span: *mut OtelSpan = unsafe {
        otel_tracer_start_span(
            tracer,
            sv(names[usize::from(spec.name_index) % 4]),
            &options,
        )
    };
    if span.is_null() {
        return;
    }
    let attribute_count = usize::from(spec.attribute_count % 4);
    for index in 0..attribute_count {
        if index % 2 == 0 {
            let _ = unsafe { otel_span_set_string_attribute(span, sv("k"), sv("v")) };
        } else {
            let _ = unsafe { otel_span_set_int64_attribute(span, sv("n"), index as i64) };
        }
    }
    if spec.add_event {
        let event_attribute_count = usize::from(spec.event_attribute_count % 4);
        let attributes: Vec<OtelKeyValue> = (0..event_attribute_count)
            .map(|index| OtelKeyValue {
                key: sv("e"),
                value_type: 2, // int64
                value: opentelemetry_c_abi::OtelAttributeValue {
                    int64_value: index as i64,
                },
            })
            .collect();
        let (ptr, count) = if attributes.is_empty() {
            (ptr::null(), 0usize)
        } else {
            (attributes.as_ptr(), attributes.len())
        };
        let _ = unsafe { otel_span_add_event(span, sv("event"), ptr, count) };
    }
    unsafe {
        otel_span_end(span);
        otel_span_destroy(span);
    }
}

fuzz_target!(|input: Input| {
    let state = State {
        callback_status: input.callback_status % 10,
        exports: AtomicUsize::new(0),
        destroys: AtomicUsize::new(0),
    };
    let callbacks = OtelCustomTraceExporterCallbacks {
        struct_size: prefix_size(
            input.callback_size,
            std::mem::size_of::<OtelCustomTraceExporterCallbacks>(),
        ),
        export_spans: input.include_export.then_some(export_spans),
        force_flush: input.include_force_flush.then_some(force_flush_state),
        shutdown: input.include_shutdown.then_some(shutdown_state),
        state_destroy: input.include_destroy.then_some(destroy_state),
    };
    let mut exporter: *mut OtelTraceExporter = ptr::null_mut();
    let status = unsafe {
        otel_custom_trace_exporter_new(
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

    let mut processor: *mut OtelSpanProcessor = ptr::null_mut();
    if unsafe { otel_simple_span_processor_create(exporter, &mut processor) } != OtelStatus::Ok {
        unsafe { otel_trace_exporter_destroy(exporter) };
        return;
    }
    let builder = otel_sdk_builder_new();
    if unsafe { otel_sdk_builder_add_span_processor(builder, processor) } != OtelStatus::Ok {
        unsafe { otel_sdk_builder_destroy(builder) };
        return;
    }
    let mut sdk: *mut OtelSdk = ptr::null_mut();
    if unsafe { otel_sdk_build(builder, &mut sdk) } != OtelStatus::Ok {
        unsafe { otel_sdk_builder_destroy(builder) };
        return;
    }
    unsafe { otel_sdk_builder_destroy(builder) };

    let provider = unsafe { otel_sdk_get_tracer_provider(sdk) }.cast::<OtelTracerProvider>();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, sv("fuzz"), sv("1.0"), sv("")) };
    if !tracer.is_null() {
        // The simple span processor exports synchronously when each span ends, so no flush is
        // needed; a timed force-flush would race the deterministic teardown below.
        for spec in input.spans.iter().take(16) {
            emit(tracer, spec);
        }
    }

    unsafe {
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
        let _ = otel_sdk_shutdown(sdk, 1_000);
        otel_sdk_destroy(sdk);
    }
    let state_destroy_end = std::mem::offset_of!(OtelCustomTraceExporterCallbacks, state_destroy)
        + std::mem::size_of_val(&callbacks.state_destroy);
    let expected_destroys =
        usize::from(input.include_destroy && callbacks.struct_size >= state_destroy_end);
    assert_eq!(state.destroys.load(Ordering::Relaxed), expected_destroys);
});
