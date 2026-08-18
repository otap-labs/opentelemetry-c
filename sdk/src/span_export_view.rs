// SPDX-License-Identifier: Apache-2.0

//! Callback-scoped, read-only C view of an exported trace span batch.

use std::os::raw::{c_char, c_void};
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry::trace::{SpanContext, SpanKind, Status};
use opentelemetry::{Array, InstrumentationScope, KeyValue, Value};
use opentelemetry_c_abi::{OtelAttributeValue, OtelBool, OtelStatus, OtelStringView};
use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};
use opentelemetry_sdk::Resource;

pub const OTEL_SPAN_EXPORT_MAX_SPANS: usize = 65_536;
pub const OTEL_SPAN_ATTRIBUTE_STRING_ARRAY: u32 = 4;
pub const OTEL_SPAN_ATTRIBUTE_BOOL_ARRAY: u32 = 5;
pub const OTEL_SPAN_ATTRIBUTE_INT64_ARRAY: u32 = 6;
pub const OTEL_SPAN_ATTRIBUTE_DOUBLE_ARRAY: u32 = 7;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanArrayView {
    pub values: *const c_void,
    pub count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OtelSpanAttributeValue {
    pub scalar: OtelAttributeValue,
    pub array: OtelSpanArrayView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanAttribute {
    pub key: OtelStringView,
    pub value_type: u32,
    pub value: OtelSpanAttributeValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanEventView {
    pub struct_size: u64,
    pub name: OtelStringView,
    pub timestamp_unix_nanos: u64,
    pub attributes: *const OtelSpanAttribute,
    pub attribute_count: usize,
    pub dropped_attributes_count: u32,
    pub reserved_flags: u32,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanExportLinkView {
    pub struct_size: u64,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub trace_flags: u8,
    pub reserved_padding: [u8; 3],
    pub is_remote: OtelBool,
    pub trace_state: OtelStringView,
    pub attributes: *const OtelSpanAttribute,
    pub attribute_count: usize,
    pub dropped_attributes_count: u32,
    pub reserved_flags: u32,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanExportScopeView {
    pub struct_size: u64,
    pub name: OtelStringView,
    pub version: OtelStringView,
    pub schema_url: OtelStringView,
    pub attributes: *const OtelSpanAttribute,
    pub attribute_count: usize,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanExportRecordView {
    pub struct_size: u64,
    pub name: OtelStringView,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: [u8; 8],
    pub trace_flags: u8,
    pub reserved_padding: [u8; 3],
    pub is_remote: OtelBool,
    pub span_kind: u32,
    pub status_code: u32,
    pub start_time_unix_nanos: u64,
    pub end_time_unix_nanos: u64,
    pub status_message: OtelStringView,
    pub trace_state: OtelStringView,
    pub attributes: *const OtelSpanAttribute,
    pub attribute_count: usize,
    pub events: *const OtelSpanEventView,
    pub event_count: usize,
    pub links: *const OtelSpanExportLinkView,
    pub link_count: usize,
    pub dropped_attributes_count: u32,
    pub dropped_events_count: u32,
    pub dropped_links_count: u32,
    pub reserved_flags: u32,
    pub scope: *const OtelSpanExportScopeView,
    pub reserved: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanExportBatchView {
    pub struct_size: u64,
    pub records: *const OtelSpanExportRecordView,
    pub record_count: usize,
    pub resource_schema_url: OtelStringView,
    pub resource_attributes: *const OtelSpanAttribute,
    pub resource_attribute_count: usize,
    pub reserved: [u64; 4],
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{align_of, size_of};
    assert!(size_of::<OtelSpanArrayView>() == 16);
    assert!(align_of::<OtelSpanArrayView>() == 8);
    assert!(size_of::<OtelSpanAttributeValue>() == 16);
    assert!(align_of::<OtelSpanAttributeValue>() == 8);
    assert!(size_of::<OtelSpanAttribute>() == 40);
    assert!(align_of::<OtelSpanAttribute>() == 8);
    assert!(size_of::<OtelSpanEventView>() == 72);
    assert!(align_of::<OtelSpanEventView>() == 8);
    assert!(size_of::<OtelSpanExportLinkView>() == 96);
    assert!(align_of::<OtelSpanExportLinkView>() == 8);
    assert!(size_of::<OtelSpanExportScopeView>() == 88);
    assert!(align_of::<OtelSpanExportScopeView>() == 8);
    assert!(size_of::<OtelSpanExportRecordView>() == 224);
    assert!(align_of::<OtelSpanExportRecordView>() == 8);
    assert!(size_of::<OtelSpanExportBatchView>() == 88);
    assert!(align_of::<OtelSpanExportBatchView>() == 8);
};

#[derive(Debug)]
pub(crate) struct ConversionError {
    pub(crate) status: OtelStatus,
    pub(crate) message: String,
}

impl ConversionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            status: OtelStatus::ExportFailed,
            message: message.into(),
        }
    }
}

type ConversionResult<T> = Result<T, ConversionError>;

#[derive(Default)]
struct ConvertedAttributes {
    attributes: Vec<OtelSpanAttribute>,
    bool_arrays: Vec<Vec<OtelBool>>,
    string_arrays: Vec<Vec<OtelStringView>>,
}

pub(crate) struct SpanExportViewStorage {
    resource_attributes: ConvertedAttributes,
    scope_attributes: Vec<ConvertedAttributes>,
    record_attributes: Vec<ConvertedAttributes>,
    event_attributes: Vec<Vec<ConvertedAttributes>>,
    link_attributes: Vec<Vec<ConvertedAttributes>>,
    events: Vec<Vec<OtelSpanEventView>>,
    links: Vec<Vec<OtelSpanExportLinkView>>,
    scopes: Vec<OtelSpanExportScopeView>,
    records: Vec<OtelSpanExportRecordView>,
    record_trace_states: Vec<String>,
    link_trace_states: Vec<Vec<String>>,
    batch: OtelSpanExportBatchView,
}

impl std::fmt::Debug for SpanExportViewStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpanExportViewStorage")
            .field("record_count", &self.records.len())
            .finish_non_exhaustive()
    }
}

impl SpanExportViewStorage {
    pub(crate) fn view(&self) -> *const OtelSpanExportBatchView {
        &self.batch
    }
}

fn empty_view() -> OtelStringView {
    OtelStringView {
        ptr: std::ptr::null(),
        len: 0,
    }
}

fn string_view(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

fn optional_string_view(value: Option<&str>) -> OtelStringView {
    value.map_or_else(empty_view, string_view)
}

fn try_push<T>(values: &mut Vec<T>, value: T, what: &str) -> ConversionResult<()> {
    values
        .try_reserve(1)
        .map_err(|_| ConversionError::new(format!("failed to allocate an exported span {what}")))?;
    values.push(value);
    Ok(())
}

fn timestamp_nanos(value: SystemTime, what: &str) -> ConversionResult<u64> {
    let nanos = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ConversionError::new(format!("exported span {what} predates the Unix epoch")))?
        .as_nanos();
    u64::try_from(nanos)
        .map_err(|_| ConversionError::new(format!("exported span {what} exceeds the u64 range")))
}

fn span_kind(kind: SpanKind) -> u32 {
    match kind {
        SpanKind::Internal => 0,
        SpanKind::Server => 1,
        SpanKind::Client => 2,
        SpanKind::Producer => 3,
        SpanKind::Consumer => 4,
    }
}

fn status_parts(status: &Status) -> (u32, OtelStringView) {
    match status {
        Status::Unset => (0, empty_view()),
        Status::Ok => (1, empty_view()),
        Status::Error { description } => (2, string_view(description.as_ref())),
    }
}

fn span_context_parts(context: &SpanContext) -> ([u8; 16], [u8; 8], u8, OtelBool, String) {
    (
        context.trace_id().to_bytes(),
        context.span_id().to_bytes(),
        context.trace_flags().to_u8(),
        u32::from(context.is_remote()),
        context.trace_state().header(),
    )
}

fn convert_attribute_parts<'a>(
    key: &'a str,
    value: &'a Value,
    out: &mut ConvertedAttributes,
) -> ConversionResult<()> {
    let (value_type, value) = match value {
        Value::String(value) => (
            0,
            OtelSpanAttributeValue {
                scalar: OtelAttributeValue {
                    string_value: string_view(value.as_str()),
                },
            },
        ),
        Value::Bool(value) => (
            1,
            OtelSpanAttributeValue {
                scalar: OtelAttributeValue {
                    bool_value: u32::from(*value),
                },
            },
        ),
        Value::I64(value) => (
            2,
            OtelSpanAttributeValue {
                scalar: OtelAttributeValue {
                    int64_value: *value,
                },
            },
        ),
        Value::F64(value) => (
            3,
            OtelSpanAttributeValue {
                scalar: OtelAttributeValue {
                    double_value: *value,
                },
            },
        ),
        Value::Array(Array::String(values)) => {
            let mut views = Vec::new();
            views.try_reserve_exact(values.len()).map_err(|_| {
                ConversionError::new("failed to allocate an exported span string attribute array")
            })?;
            views.extend(values.iter().map(|value| string_view(value.as_str())));
            try_push(
                &mut out.string_arrays,
                views,
                "string attribute array backing",
            )?;
            let values = out.string_arrays.last().expect("just pushed");
            (
                OTEL_SPAN_ATTRIBUTE_STRING_ARRAY,
                OtelSpanAttributeValue {
                    array: OtelSpanArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
            )
        }
        Value::Array(Array::Bool(values)) => {
            let mut bools = Vec::new();
            bools.try_reserve_exact(values.len()).map_err(|_| {
                ConversionError::new("failed to allocate an exported span bool attribute array")
            })?;
            bools.extend(values.iter().map(|value| u32::from(*value)));
            try_push(&mut out.bool_arrays, bools, "bool attribute array backing")?;
            let values = out.bool_arrays.last().expect("just pushed");
            (
                OTEL_SPAN_ATTRIBUTE_BOOL_ARRAY,
                OtelSpanAttributeValue {
                    array: OtelSpanArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
            )
        }
        Value::Array(Array::I64(values)) => (
            OTEL_SPAN_ATTRIBUTE_INT64_ARRAY,
            OtelSpanAttributeValue {
                array: OtelSpanArrayView {
                    values: values.as_ptr().cast(),
                    count: values.len(),
                },
            },
        ),
        Value::Array(Array::F64(values)) => (
            OTEL_SPAN_ATTRIBUTE_DOUBLE_ARRAY,
            OtelSpanAttributeValue {
                array: OtelSpanArrayView {
                    values: values.as_ptr().cast(),
                    count: values.len(),
                },
            },
        ),
        _ => unreachable!("all OpenTelemetry attribute variants are handled"),
    };
    try_push(
        &mut out.attributes,
        OtelSpanAttribute {
            key: string_view(key),
            value_type,
            value,
        },
        "attribute",
    )
}

fn convert_key_values<'a>(
    values: impl Iterator<Item = &'a KeyValue>,
) -> ConversionResult<ConvertedAttributes> {
    let mut out = ConvertedAttributes::default();
    for attribute in values {
        convert_attribute_parts(attribute.key.as_str(), &attribute.value, &mut out)?;
    }
    Ok(out)
}

fn convert_resource_attributes(resource: &Resource) -> ConversionResult<ConvertedAttributes> {
    let mut out = ConvertedAttributes::default();
    for (key, value) in resource.iter() {
        convert_attribute_parts(key.as_str(), value, &mut out)?;
    }
    Ok(out)
}

fn convert_scope(
    scope: &InstrumentationScope,
) -> ConversionResult<(OtelSpanExportScopeView, ConvertedAttributes)> {
    let attributes = convert_key_values(scope.attributes())?;
    Ok((
        OtelSpanExportScopeView {
            struct_size: std::mem::size_of::<OtelSpanExportScopeView>() as u64,
            name: string_view(scope.name()),
            version: optional_string_view(scope.version()),
            schema_url: optional_string_view(scope.schema_url()),
            attributes: std::ptr::null(),
            attribute_count: attributes.attributes.len(),
            reserved: [0; 2],
        },
        attributes,
    ))
}

fn convert_events(
    events: &SpanEvents,
) -> ConversionResult<(Vec<OtelSpanEventView>, Vec<ConvertedAttributes>)> {
    let mut views = Vec::new();
    let mut attributes = Vec::new();
    views
        .try_reserve_exact(events.events.len())
        .map_err(|_| ConversionError::new("failed to allocate exported span event views"))?;
    attributes
        .try_reserve_exact(events.events.len())
        .map_err(|_| ConversionError::new("failed to allocate exported span event attributes"))?;
    for event in &events.events {
        let converted = convert_key_values(event.attributes.iter())?;
        views.push(OtelSpanEventView {
            struct_size: std::mem::size_of::<OtelSpanEventView>() as u64,
            name: string_view(event.name.as_ref()),
            timestamp_unix_nanos: timestamp_nanos(event.timestamp, "event timestamp")?,
            attributes: std::ptr::null(),
            attribute_count: converted.attributes.len(),
            dropped_attributes_count: event.dropped_attributes_count,
            reserved_flags: 0,
            reserved: [0; 2],
        });
        attributes.push(converted);
    }
    Ok((views, attributes))
}

fn convert_links(
    links: &SpanLinks,
) -> ConversionResult<(
    Vec<OtelSpanExportLinkView>,
    Vec<ConvertedAttributes>,
    Vec<String>,
)> {
    let mut views = Vec::new();
    let mut attributes = Vec::new();
    let mut trace_states = Vec::new();
    views
        .try_reserve_exact(links.links.len())
        .map_err(|_| ConversionError::new("failed to allocate exported span link views"))?;
    attributes
        .try_reserve_exact(links.links.len())
        .map_err(|_| ConversionError::new("failed to allocate exported span link attributes"))?;
    trace_states
        .try_reserve_exact(links.links.len())
        .map_err(|_| ConversionError::new("failed to allocate exported span link trace states"))?;
    for link in &links.links {
        let (trace_id, span_id, trace_flags, is_remote, trace_state) =
            span_context_parts(&link.span_context);
        let converted = convert_key_values(link.attributes.iter())?;
        views.push(OtelSpanExportLinkView {
            struct_size: std::mem::size_of::<OtelSpanExportLinkView>() as u64,
            trace_id,
            span_id,
            trace_flags,
            reserved_padding: [0; 3],
            is_remote,
            trace_state: empty_view(),
            attributes: std::ptr::null(),
            attribute_count: converted.attributes.len(),
            dropped_attributes_count: link.dropped_attributes_count,
            reserved_flags: 0,
            reserved: [0; 2],
        });
        attributes.push(converted);
        trace_states.push(trace_state);
    }
    Ok((views, attributes, trace_states))
}

pub(crate) fn convert_batch(
    spans: &[SpanData],
    resource: &Resource,
) -> ConversionResult<Box<SpanExportViewStorage>> {
    if spans.len() > OTEL_SPAN_EXPORT_MAX_SPANS {
        return Err(ConversionError::new(format!(
            "exported span batch carries more than {OTEL_SPAN_EXPORT_MAX_SPANS} spans"
        )));
    }

    let resource_attributes = convert_resource_attributes(resource)?;
    let mut scope_attributes = Vec::new();
    let mut record_attributes = Vec::new();
    let mut event_attributes = Vec::new();
    let mut link_attributes = Vec::new();
    let mut events = Vec::new();
    let mut links = Vec::new();
    let mut scopes = Vec::new();
    let mut records = Vec::new();
    let mut record_trace_states = Vec::new();
    let mut link_trace_states = Vec::new();

    for span in spans {
        let (scope, scope_attrs) = convert_scope(&span.instrumentation_scope)?;
        let record_attrs = convert_key_values(span.attributes.iter())?;
        let (event_views, event_attrs) = convert_events(&span.events)?;
        let (link_views, link_attrs, link_states) = convert_links(&span.links)?;
        let (trace_id, span_id, trace_flags, is_remote, trace_state) =
            span_context_parts(&span.span_context);
        let (status_code, status_message) = status_parts(&span.status);
        let record = OtelSpanExportRecordView {
            struct_size: std::mem::size_of::<OtelSpanExportRecordView>() as u64,
            name: string_view(span.name.as_ref()),
            trace_id,
            span_id,
            parent_span_id: span.parent_span_id.to_bytes(),
            trace_flags,
            reserved_padding: [0; 3],
            is_remote,
            span_kind: span_kind(span.span_kind.clone()),
            status_code,
            start_time_unix_nanos: timestamp_nanos(span.start_time, "start time")?,
            end_time_unix_nanos: timestamp_nanos(span.end_time, "end time")?,
            status_message,
            trace_state: empty_view(),
            attributes: std::ptr::null(),
            attribute_count: record_attrs.attributes.len(),
            events: std::ptr::null(),
            event_count: event_views.len(),
            links: std::ptr::null(),
            link_count: link_views.len(),
            dropped_attributes_count: span.dropped_attributes_count,
            dropped_events_count: span.events.dropped_count,
            dropped_links_count: span.links.dropped_count,
            reserved_flags: 0,
            scope: std::ptr::null(),
            reserved: [0; 4],
        };
        try_push(&mut scopes, scope, "scope view")?;
        try_push(&mut scope_attributes, scope_attrs, "scope attributes")?;
        try_push(&mut records, record, "record view")?;
        try_push(&mut record_attributes, record_attrs, "record attributes")?;
        try_push(&mut events, event_views, "event views")?;
        try_push(&mut event_attributes, event_attrs, "event attributes")?;
        try_push(&mut links, link_views, "link views")?;
        try_push(&mut link_attributes, link_attrs, "link attributes")?;
        try_push(&mut record_trace_states, trace_state, "trace state")?;
        try_push(&mut link_trace_states, link_states, "link trace states")?;
    }

    let mut storage = Box::new(SpanExportViewStorage {
        resource_attributes,
        scope_attributes,
        record_attributes,
        event_attributes,
        link_attributes,
        events,
        links,
        scopes,
        records,
        record_trace_states,
        link_trace_states,
        batch: OtelSpanExportBatchView {
            struct_size: std::mem::size_of::<OtelSpanExportBatchView>() as u64,
            records: std::ptr::null(),
            record_count: 0,
            resource_schema_url: optional_string_view(resource.schema_url()),
            resource_attributes: std::ptr::null(),
            resource_attribute_count: 0,
            reserved: [0; 4],
        },
    });

    for (index, scope) in storage.scopes.iter_mut().enumerate() {
        scope.attributes = storage.scope_attributes[index].attributes.as_ptr();
    }
    let scopes_base = storage.scopes.as_ptr();
    for (record_index, record) in storage.records.iter_mut().enumerate() {
        record.attributes = storage.record_attributes[record_index].attributes.as_ptr();
        record.events = storage.events[record_index].as_ptr();
        record.links = storage.links[record_index].as_ptr();
        record.trace_state = string_view(&storage.record_trace_states[record_index]);
        record.scope = unsafe { scopes_base.add(record_index) };
        for (event_index, event) in storage.events[record_index].iter_mut().enumerate() {
            event.attributes = storage.event_attributes[record_index][event_index]
                .attributes
                .as_ptr();
        }
        for (link_index, link) in storage.links[record_index].iter_mut().enumerate() {
            link.attributes = storage.link_attributes[record_index][link_index]
                .attributes
                .as_ptr();
            link.trace_state = string_view(&storage.link_trace_states[record_index][link_index]);
        }
    }
    storage.batch.records = storage.records.as_ptr();
    storage.batch.record_count = storage.records.len();
    storage.batch.resource_attributes = storage.resource_attributes.attributes.as_ptr();
    storage.batch.resource_attribute_count = storage.resource_attributes.attributes.len();
    Ok(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{Event, Link, SpanContext, SpanId, TraceFlags, TraceId, TraceState};
    use opentelemetry::{Array, KeyValue};
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};
    use std::borrow::Cow;
    use std::time::{Duration, UNIX_EPOCH};

    fn text(view: OtelStringView) -> &'static str {
        if view.len == 0 {
            return "";
        }
        unsafe {
            std::str::from_utf8(std::slice::from_raw_parts(view.ptr.cast::<u8>(), view.len))
                .unwrap()
        }
    }

    fn span() -> SpanData {
        SpanData {
            span_context: SpanContext::new(
                TraceId::from_bytes([1; 16]),
                SpanId::from_bytes([2; 8]),
                TraceFlags::SAMPLED,
                true,
                TraceState::default(),
            ),
            parent_span_id: SpanId::from_bytes([3; 8]),
            parent_span_is_remote: false,
            span_kind: SpanKind::Client,
            name: Cow::Borrowed("span-name"),
            start_time: UNIX_EPOCH + Duration::from_nanos(10),
            end_time: UNIX_EPOCH + Duration::from_nanos(20),
            attributes: vec![KeyValue::new(
                "numbers",
                Value::Array(Array::I64(vec![1, 2])),
            )],
            dropped_attributes_count: 1,
            events: {
                let mut events = SpanEvents::default();
                events.events = vec![Event::new(
                    "event",
                    UNIX_EPOCH + Duration::from_nanos(15),
                    vec![KeyValue::new("ok", true)],
                    2,
                )];
                events.dropped_count = 3;
                events
            },
            links: {
                let mut links = SpanLinks::default();
                links.links = vec![Link::new(
                    SpanContext::new(
                        TraceId::from_bytes([4; 16]),
                        SpanId::from_bytes([5; 8]),
                        TraceFlags::default(),
                        false,
                        TraceState::default(),
                    ),
                    vec![KeyValue::new("link", "yes")],
                    4,
                )];
                links.dropped_count = 5;
                links
            },
            status: Status::error("bad"),
            instrumentation_scope: InstrumentationScope::builder("scope")
                .with_version("1.0")
                .with_attributes([KeyValue::new("scope.attr", 7_i64)])
                .build(),
        }
    }

    #[test]
    fn converts_span_batch() {
        let resource = Resource::builder_empty()
            .with_attributes([KeyValue::new("service.name", "svc")])
            .build();
        let spans = [span()];
        let storage = convert_batch(&spans, &resource).unwrap();
        let batch = unsafe { &*storage.view() };
        assert_eq!(batch.record_count, 1);
        assert_eq!(batch.resource_attribute_count, 1);
        let record = unsafe { &*batch.records };
        assert_eq!(text(record.name), "span-name");
        assert_eq!(record.span_kind, 2);
        assert_eq!(record.status_code, 2);
        assert_eq!(text(record.status_message), "bad");
        assert_eq!(record.attribute_count, 1);
        assert_eq!(record.event_count, 1);
        assert_eq!(record.link_count, 1);
        assert_eq!(unsafe { (*record.attributes).value.array.count }, 2);
        assert_eq!(text(unsafe { (*record.events).name }), "event");
        assert_eq!(unsafe { (*record.links).span_id }, [5; 8]);
        assert_eq!(text(unsafe { (*record.scope).name }), "scope");
    }

    #[test]
    fn rejects_oversized_batches() {
        let spans = vec![span(); OTEL_SPAN_EXPORT_MAX_SPANS + 1];
        let error = convert_batch(&spans, &Resource::builder_empty().build()).unwrap_err();
        assert_eq!(error.status, OtelStatus::ExportFailed);
    }
}
