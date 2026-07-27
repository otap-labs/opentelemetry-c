//! Callback-scoped, read-only C view of an exported Logs batch.
//!
//! The pinned `LogExporter::export` receives a borrowed `LogBatch`. This module converts that
//! batch into a **flat, borrowed** C representation that is valid only for the duration of a
//! single `export_logs` callback, and then throws it away.
//!
//! ## Why a flattened view instead of opaque getters
//!
//! The Logs *input* ABI (`otel_log_record_view_t`) already establishes a flat node pool with
//! `[first, first + count)` child ranges, so bridge authors in C, Perl XS, Ruby, and Python
//! already have traversal code for exactly this shape. Reusing `otel_log_value_t` and
//! `otel_log_key_value_t` lets them reuse it verbatim, keeps every discriminant and range
//! trivially fuzzable, makes reference cycles structurally impossible, and needs no recursive
//! C callback chain for attacker-controlled nesting.
//!
//! ## Pool invariants (identical to the input model unless noted)
//!
//! * a child range lies entirely within `[0, value_node_count)`;
//! * a pool node only references children at a **strictly greater** index than its own, so a
//!   cycle cannot be expressed;
//! * every pool node is referenced **exactly once**, by one container or by one root;
//! * array elements carry an empty key; map entries carry a unique key.
//!
//! The single deviation from the input model: an exported map key is reproduced exactly as
//! the upstream record supplied it and is therefore *permitted* to be empty. This is a read
//! path, so a legal upstream record is never rewritten or dropped to satisfy an input-side
//! rule.
//!
//! ## Construction strategy
//!
//! Flattening is breadth-first and iterative. A container's children are appended to the end
//! of the pool in one contiguous block at the moment the container is visited, which gives
//! both the contiguity and the strictly-greater-index invariants for free and bounds the work
//! without recursion. Every pool, attribute array, and record array is allocated with
//! `try_reserve` so a hostile count fails the export instead of aborting the process.

use std::collections::VecDeque;
use std::os::raw::c_char;
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry::logs::{AnyValue, Severity};
use opentelemetry::{Array, InstrumentationScope, Key, Value};
use opentelemetry_c_abi::{
    OtelLogBytesView, OtelLogTraceContext, OtelLogValue, OtelLogValueNode, OtelLogValuePayload,
    OtelLogValueRange, OtelLogValueType, OtelStatus, OtelStringView, OTEL_LOG_MAX_ARRAY_ELEMENTS,
    OTEL_LOG_MAX_ATTRIBUTES, OTEL_LOG_MAX_BYTES_LEN, OTEL_LOG_MAX_MAP_ENTRIES,
    OTEL_LOG_MAX_STRING_LEN, OTEL_LOG_MAX_VALUE_DEPTH, OTEL_LOG_MAX_VALUE_NODES,
};
use opentelemetry_sdk::logs::{LogBatch, SdkLogRecord};
use opentelemetry_sdk::Resource;

/// `present_fields` bit selecting `timestamp_unix_nanos`.
pub const OTEL_LOG_EXPORT_FIELD_TIMESTAMP: u64 = 1 << 0;
/// `present_fields` bit selecting `observed_timestamp_unix_nanos`.
pub const OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP: u64 = 1 << 1;
/// `present_fields` bit selecting `trace_context`.
pub const OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT: u64 = 1 << 2;
/// `present_fields` bit selecting `severity_text`.
pub const OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT: u64 = 1 << 3;
/// `present_fields` bit selecting `event_name`.
pub const OTEL_LOG_EXPORT_FIELD_EVENT_NAME: u64 = 1 << 4;
/// `present_fields` bit selecting `target`.
pub const OTEL_LOG_EXPORT_FIELD_TARGET: u64 = 1 << 5;
/// `present_fields` bit distinguishing an explicitly empty body from an absent one.
pub const OTEL_LOG_EXPORT_FIELD_BODY: u64 = 1 << 6;

/// Every `present_fields` bit this ABI version can set.
pub const OTEL_LOG_EXPORT_FIELD_KNOWN_MASK: u64 = OTEL_LOG_EXPORT_FIELD_TIMESTAMP
    | OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP
    | OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT
    | OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT
    | OTEL_LOG_EXPORT_FIELD_EVENT_NAME
    | OTEL_LOG_EXPORT_FIELD_TARGET
    | OTEL_LOG_EXPORT_FIELD_BODY;

/// Maximum number of records one export callback may observe in a single batch.
///
/// Comfortably above the largest batch the batch log processor will assemble; a batch beyond
/// it fails the export rather than being silently truncated.
pub const OTEL_LOG_EXPORT_MAX_RECORDS: usize = 65_536;

/// Borrowed instrumentation scope shared by one or more records in a batch.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogExportScopeView {
    /// `sizeof(otel_log_export_scope_view_t)` as compiled by this library.
    pub struct_size: u64,
    /// Scope name.
    pub name: OtelStringView,
    /// Scope version, empty when absent.
    pub version: OtelStringView,
    /// Scope schema URL, empty when absent.
    pub schema_url: OtelStringView,
    /// Scope attributes, in upstream order.
    pub attributes: *const OtelLogValueNode,
    /// Number of scope attributes.
    pub attribute_count: usize,
    /// Node pool addressed by this scope's attribute child ranges.
    pub value_nodes: *const OtelLogValueNode,
    /// Number of nodes in the scope pool.
    pub value_node_count: usize,
    /// Reserved; always zero.
    pub reserved: [u64; 2],
}

/// Borrowed, read-only view of one exported log record.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogExportRecordView {
    /// `sizeof(otel_log_export_record_view_t)` as compiled by this library.
    pub struct_size: u64,
    /// Bit set of fields the upstream record actually carried.
    pub present_fields: u64,
    /// Event time in nanoseconds since the Unix epoch.
    pub timestamp_unix_nanos: u64,
    /// Observation time in nanoseconds since the Unix epoch.
    pub observed_timestamp_unix_nanos: u64,
    /// Canonical severity number, or `0` when the record carried none.
    pub severity_number: u32,
    /// Reserved; always zero.
    pub reserved_flags: u32,
    /// Severity text exactly as the upstream record carried it.
    pub severity_text: OtelStringView,
    /// Event name exactly as the upstream record carried it.
    pub event_name: OtelStringView,
    /// Upstream `target`, informational only.
    pub target: OtelStringView,
    /// Record body, `Empty` when the record carried none.
    pub body: OtelLogValue,
    /// Record attributes, in upstream order with duplicate keys preserved.
    pub attributes: *const OtelLogValueNode,
    /// Number of record attributes.
    pub attribute_count: usize,
    /// Node pool addressed by this record's body/attribute child ranges.
    pub value_nodes: *const OtelLogValueNode,
    /// Number of nodes in the record pool.
    pub value_node_count: usize,
    /// Trace correlation; read only when [`OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT`] is set.
    pub trace_context: OtelLogTraceContext,
    /// Instrumentation scope this record was emitted through. Never NULL.
    pub scope: *const OtelLogExportScopeView,
    /// Reserved; always zero.
    pub reserved: [u64; 4],
}

/// Borrowed, read-only view of one exported log batch.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogExportBatchView {
    /// `sizeof(otel_log_export_batch_view_t)` as compiled by this library.
    pub struct_size: u64,
    /// Records in upstream order.
    pub records: *const OtelLogExportRecordView,
    /// Number of records.
    pub record_count: usize,
    /// Resource schema URL, empty when absent.
    pub resource_schema_url: OtelStringView,
    /// Resource attributes.
    pub resource_attributes: *const OtelLogValueNode,
    /// Number of resource attributes.
    pub resource_attribute_count: usize,
    /// Node pool addressed by the resource attributes' child ranges.
    pub resource_value_nodes: *const OtelLogValueNode,
    /// Number of nodes in the resource pool.
    pub resource_value_node_count: usize,
    /// Reserved; always zero.
    pub reserved: [u64; 4],
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{align_of, size_of};
    assert!(size_of::<OtelLogExportScopeView>() == 104);
    assert!(align_of::<OtelLogExportScopeView>() == 8);
    assert!(size_of::<OtelLogExportRecordView>() == 216);
    assert!(align_of::<OtelLogExportRecordView>() == 8);
    assert!(size_of::<OtelLogExportBatchView>() == 104);
    assert!(align_of::<OtelLogExportBatchView>() == 8);
    // The reused input value types must keep the layout the public header asserts.
    assert!(size_of::<OtelLogValue>() == 24);
    assert!(size_of::<OtelLogValueNode>() == 40);
    assert!(size_of::<OtelLogTraceContext>() == 32);
};

/// A conversion failure. Carries the public status class plus a diagnostic that names the
/// offending limit or field without reproducing telemetry content.
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

fn empty_view() -> OtelStringView {
    OtelStringView {
        ptr: std::ptr::null(),
        len: 0,
    }
}

fn string_view(value: &str) -> ConversionResult<OtelStringView> {
    if value.len() > OTEL_LOG_MAX_STRING_LEN {
        return Err(ConversionError::new(format!(
            "exported log string of {} bytes exceeds the {OTEL_LOG_MAX_STRING_LEN}-byte limit",
            value.len()
        )));
    }
    Ok(OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    })
}

fn optional_string_view(value: Option<&str>) -> ConversionResult<OtelStringView> {
    match value {
        Some(value) => string_view(value),
        None => Ok(empty_view()),
    }
}

fn scalar(value_type: OtelLogValueType, value: OtelLogValuePayload) -> OtelLogValue {
    OtelLogValue {
        value_type: value_type as u32,
        reserved: 0,
        value,
    }
}

fn empty_value() -> OtelLogValue {
    scalar(
        OtelLogValueType::Empty,
        OtelLogValuePayload {
            string_value: empty_view(),
        },
    )
}

fn empty_node() -> OtelLogValueNode {
    OtelLogValueNode {
        key: empty_view(),
        value: empty_value(),
    }
}

fn try_sized_vec<T: Clone>(value: T, len: usize) -> ConversionResult<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(len).map_err(|_| {
        ConversionError::new(format!(
            "failed to allocate {len} elements for the exported log batch view"
        ))
    })?;
    values.resize(len, value);
    Ok(values)
}

fn timestamp_nanos(value: SystemTime, what: &str) -> ConversionResult<u64> {
    let nanos = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ConversionError::new(format!("exported log {what} predates the Unix epoch")))?
        .as_nanos();
    u64::try_from(nanos)
        .map_err(|_| ConversionError::new(format!("exported log {what} exceeds the u64 range")))
}

/// Map the pinned `Severity` enum onto the canonical C severity number.
///
/// Written as an explicit match rather than an `as` cast so a change to the pinned
/// discriminants becomes a compile error instead of silent corruption.
fn severity_number(severity: Severity) -> u32 {
    match severity {
        Severity::Trace => 1,
        Severity::Trace2 => 2,
        Severity::Trace3 => 3,
        Severity::Trace4 => 4,
        Severity::Debug => 5,
        Severity::Debug2 => 6,
        Severity::Debug3 => 7,
        Severity::Debug4 => 8,
        Severity::Info => 9,
        Severity::Info2 => 10,
        Severity::Info3 => 11,
        Severity::Info4 => 12,
        Severity::Warn => 13,
        Severity::Warn2 => 14,
        Severity::Warn3 => 15,
        Severity::Warn4 => 16,
        Severity::Error => 17,
        Severity::Error2 => 18,
        Severity::Error3 => 19,
        Severity::Error4 => 20,
        Severity::Fatal => 21,
        Severity::Fatal2 => 22,
        Severity::Fatal3 => 23,
        Severity::Fatal4 => 24,
    }
}

/// Where a converted value is written once it is dequeued.
#[derive(Clone, Copy)]
enum Slot {
    Body,
    Attribute(usize),
    Node(usize),
}

/// A borrowed source value.
///
/// Record bodies and attributes use the Logs `AnyValue` model; resource and
/// instrumentation-scope attributes use the narrower scalar/array `Value` model. Both are
/// presented to C through the same node pool so a bridge needs exactly one traversal.
#[derive(Clone, Copy)]
enum Source<'a> {
    Any(&'a AnyValue),
    Attribute(&'a Value),
    /// One element of an attribute array, already reduced to a scalar log value.
    Scalar(OtelLogValue),
}

/// Children discovered while converting one container, queued after the container's own slot
/// has been written.
enum ContainerChildren<'a> {
    List(&'a [AnyValue]),
    Map(Vec<(&'a Key, &'a AnyValue)>),
    Scalars(Vec<OtelLogValue>),
}

/// The flattened result for one owner (record, scope, or resource).
struct Flattened {
    body: OtelLogValue,
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
}

/// Breadth-first flattener producing one record-, scope-, or resource-scoped node pool.
struct Flattener<'a> {
    nodes: Vec<OtelLogValueNode>,
    queue: VecDeque<(Slot, Source<'a>, usize)>,
}

impl<'a> Flattener<'a> {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            queue: VecDeque::new(),
        }
    }

    /// Append `count` placeholder children and return their contiguous range.
    ///
    /// The range always starts past every node already in the pool, which is what gives the
    /// strictly-greater-index invariant.
    fn reserve_children(&mut self, count: usize) -> ConversionResult<OtelLogValueRange> {
        let first = u32::try_from(self.nodes.len()).map_err(|_| {
            ConversionError::new("exported log value node index exceeds the 32-bit pool range")
        })?;
        let count32 = u32::try_from(count).map_err(|_| {
            ConversionError::new("exported log container size exceeds the 32-bit pool range")
        })?;
        let end = self.nodes.len().checked_add(count).ok_or_else(|| {
            ConversionError::new("exported log value node count overflowed the address space")
        })?;
        if end > OTEL_LOG_MAX_VALUE_NODES {
            return Err(ConversionError::new(format!(
                "exported log value needs {end} nodes, above the \
                 {OTEL_LOG_MAX_VALUE_NODES}-node limit"
            )));
        }
        self.nodes.try_reserve(count).map_err(|_| {
            ConversionError::new(format!(
                "failed to allocate {count} exported log value nodes"
            ))
        })?;
        self.nodes.resize(end, empty_node());
        Ok(OtelLogValueRange {
            first,
            count: count32,
        })
    }

    fn enqueue(&mut self, slot: Slot, source: Source<'a>, depth: usize) -> ConversionResult<()> {
        if depth > OTEL_LOG_MAX_VALUE_DEPTH {
            return Err(ConversionError::new(format!(
                "exported log value nesting exceeds the {OTEL_LOG_MAX_VALUE_DEPTH}-level limit"
            )));
        }
        self.queue.try_reserve(1).map_err(|_| {
            ConversionError::new("failed to allocate the exported log conversion work queue")
        })?;
        self.queue.push_back((slot, source, depth));
        Ok(())
    }

    fn convert_any(
        &mut self,
        value: &'a AnyValue,
        pending: &mut Vec<(OtelLogValueRange, ContainerChildren<'a>)>,
    ) -> ConversionResult<OtelLogValue> {
        Ok(match value {
            AnyValue::Int(value) => scalar(
                OtelLogValueType::Int64,
                OtelLogValuePayload {
                    int64_value: *value,
                },
            ),
            AnyValue::Double(value) => scalar(
                OtelLogValueType::Double,
                OtelLogValuePayload {
                    double_value: *value,
                },
            ),
            AnyValue::Boolean(value) => scalar(
                OtelLogValueType::Bool,
                OtelLogValuePayload {
                    bool_value: u32::from(*value),
                },
            ),
            AnyValue::String(value) => scalar(
                OtelLogValueType::String,
                OtelLogValuePayload {
                    string_value: string_view(value.as_str())?,
                },
            ),
            AnyValue::Bytes(value) => {
                if value.len() > OTEL_LOG_MAX_BYTES_LEN {
                    return Err(ConversionError::new(format!(
                        "exported log byte value of {} bytes exceeds the \
                         {OTEL_LOG_MAX_BYTES_LEN}-byte limit",
                        value.len()
                    )));
                }
                scalar(
                    OtelLogValueType::Bytes,
                    OtelLogValuePayload {
                        bytes_value: OtelLogBytesView {
                            ptr: value.as_ptr(),
                            len: value.len(),
                        },
                    },
                )
            }
            AnyValue::ListAny(values) => {
                if values.len() > OTEL_LOG_MAX_ARRAY_ELEMENTS {
                    return Err(ConversionError::new(format!(
                        "exported log array of {} elements exceeds the \
                         {OTEL_LOG_MAX_ARRAY_ELEMENTS}-element limit",
                        values.len()
                    )));
                }
                let range = self.reserve_children(values.len())?;
                pending.push((range, ContainerChildren::List(values.as_slice())));
                scalar(
                    OtelLogValueType::Array,
                    OtelLogValuePayload { children: range },
                )
            }
            AnyValue::Map(entries) => {
                if entries.len() > OTEL_LOG_MAX_MAP_ENTRIES {
                    return Err(ConversionError::new(format!(
                        "exported log map of {} entries exceeds the \
                         {OTEL_LOG_MAX_MAP_ENTRIES}-entry limit",
                        entries.len()
                    )));
                }
                // The pinned map is a `HashMap`, whose iteration order is not stable across
                // runs. Sorting by key makes every export deterministic for bridge authors,
                // tests, and fuzzing without altering the data.
                let mut sorted: Vec<(&Key, &AnyValue)> = Vec::new();
                sorted.try_reserve_exact(entries.len()).map_err(|_| {
                    ConversionError::new("failed to allocate an exported log map ordering")
                })?;
                sorted.extend(entries.iter());
                sorted.sort_unstable_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
                let range = self.reserve_children(sorted.len())?;
                pending.push((range, ContainerChildren::Map(sorted)));
                scalar(
                    OtelLogValueType::Map,
                    OtelLogValuePayload { children: range },
                )
            }
            // `AnyValue` is `#[non_exhaustive]`. Failing the export is deliberate: silently
            // substituting an empty value would hand bridge authors corrupted telemetry with
            // no signal that a newer upstream value kind went missing.
            _ => {
                return Err(ConversionError::new(
                    "exported log value uses a value kind that this ABI version cannot represent",
                ))
            }
        })
    }

    fn convert_attribute_array(&mut self, array: &'a Array) -> ConversionResult<Vec<OtelLogValue>> {
        let mut elements: Vec<OtelLogValue> = Vec::new();
        let reserve = |elements: &mut Vec<OtelLogValue>, len: usize| -> ConversionResult<()> {
            elements
                .try_reserve_exact(len)
                .map_err(|_| ConversionError::new("failed to allocate an exported array attribute"))
        };
        match array {
            Array::Bool(values) => {
                reserve(&mut elements, values.len())?;
                for value in values {
                    elements.push(scalar(
                        OtelLogValueType::Bool,
                        OtelLogValuePayload {
                            bool_value: u32::from(*value),
                        },
                    ));
                }
            }
            Array::I64(values) => {
                reserve(&mut elements, values.len())?;
                for value in values {
                    elements.push(scalar(
                        OtelLogValueType::Int64,
                        OtelLogValuePayload {
                            int64_value: *value,
                        },
                    ));
                }
            }
            Array::F64(values) => {
                reserve(&mut elements, values.len())?;
                for value in values {
                    elements.push(scalar(
                        OtelLogValueType::Double,
                        OtelLogValuePayload {
                            double_value: *value,
                        },
                    ));
                }
            }
            Array::String(values) => {
                reserve(&mut elements, values.len())?;
                for value in values {
                    elements.push(scalar(
                        OtelLogValueType::String,
                        OtelLogValuePayload {
                            string_value: string_view(value.as_str())?,
                        },
                    ));
                }
            }
            _ => {
                return Err(ConversionError::new(
                    "exported attribute array uses an unsupported element type",
                ))
            }
        }
        Ok(elements)
    }

    fn convert_attribute_value(
        &mut self,
        value: &'a Value,
        pending: &mut Vec<(OtelLogValueRange, ContainerChildren<'a>)>,
    ) -> ConversionResult<OtelLogValue> {
        Ok(match value {
            Value::Bool(value) => scalar(
                OtelLogValueType::Bool,
                OtelLogValuePayload {
                    bool_value: u32::from(*value),
                },
            ),
            Value::I64(value) => scalar(
                OtelLogValueType::Int64,
                OtelLogValuePayload {
                    int64_value: *value,
                },
            ),
            Value::F64(value) => scalar(
                OtelLogValueType::Double,
                OtelLogValuePayload {
                    double_value: *value,
                },
            ),
            Value::String(value) => scalar(
                OtelLogValueType::String,
                OtelLogValuePayload {
                    string_value: string_view(value.as_str())?,
                },
            ),
            Value::Array(array) => {
                let elements = self.convert_attribute_array(array)?;
                if elements.len() > OTEL_LOG_MAX_ARRAY_ELEMENTS {
                    return Err(ConversionError::new(format!(
                        "exported attribute array of {} elements exceeds the \
                         {OTEL_LOG_MAX_ARRAY_ELEMENTS}-element limit",
                        elements.len()
                    )));
                }
                let range = self.reserve_children(elements.len())?;
                pending.push((range, ContainerChildren::Scalars(elements)));
                scalar(
                    OtelLogValueType::Array,
                    OtelLogValuePayload { children: range },
                )
            }
            _ => {
                return Err(ConversionError::new(
                    "exported attribute uses an unsupported value type",
                ))
            }
        })
    }

    /// Flatten one owner's body and attributes into a self-contained pool.
    fn flatten(
        mut self,
        body: Option<&'a AnyValue>,
        attributes: &[(&'a Key, Source<'a>)],
    ) -> ConversionResult<Flattened> {
        if attributes.len() > OTEL_LOG_MAX_ATTRIBUTES {
            return Err(ConversionError::new(format!(
                "exported log owner carries {} attributes, above the \
                 {OTEL_LOG_MAX_ATTRIBUTES}-attribute limit",
                attributes.len()
            )));
        }
        let mut attribute_nodes = try_sized_vec(empty_node(), attributes.len())?;
        if let Some(body) = body {
            self.enqueue(Slot::Body, Source::Any(body), 1)?;
        }
        for (index, (key, source)) in attributes.iter().enumerate() {
            attribute_nodes[index].key = string_view(key.as_str())?;
            self.enqueue(Slot::Attribute(index), *source, 1)?;
        }

        let mut body_value = empty_value();
        let mut pending: Vec<(OtelLogValueRange, ContainerChildren<'a>)> = Vec::new();
        while let Some((slot, source, depth)) = self.queue.pop_front() {
            let value = match source {
                Source::Any(value) => self.convert_any(value, &mut pending)?,
                Source::Attribute(value) => self.convert_attribute_value(value, &mut pending)?,
                Source::Scalar(value) => value,
            };
            match slot {
                Slot::Body => body_value = value,
                Slot::Attribute(index) => attribute_nodes[index].value = value,
                Slot::Node(index) => self.nodes[index].value = value,
            }
            for (range, children) in pending.drain(..) {
                let first = range.first as usize;
                match children {
                    ContainerChildren::List(values) => {
                        for (offset, value) in values.iter().enumerate() {
                            self.enqueue(
                                Slot::Node(first + offset),
                                Source::Any(value),
                                depth + 1,
                            )?;
                        }
                    }
                    ContainerChildren::Map(entries) => {
                        for (offset, (key, value)) in entries.into_iter().enumerate() {
                            self.nodes[first + offset].key = string_view(key.as_str())?;
                            self.enqueue(
                                Slot::Node(first + offset),
                                Source::Any(value),
                                depth + 1,
                            )?;
                        }
                    }
                    ContainerChildren::Scalars(values) => {
                        for (offset, value) in values.into_iter().enumerate() {
                            self.enqueue(
                                Slot::Node(first + offset),
                                Source::Scalar(value),
                                depth + 1,
                            )?;
                        }
                    }
                }
            }
        }

        Ok(Flattened {
            body: body_value,
            attributes: attribute_nodes,
            nodes: self.nodes,
        })
    }
}

/// Owned backing storage for one export callback.
///
/// Every pointer handed to C addresses memory owned by this structure, which is dropped as
/// soon as the callback returns. Per-owner `Vec`s are used (rather than one flat arena) so
/// that growing the record/scope vectors never moves a buffer another view already points at.
pub(crate) struct ExportViewStorage {
    resource_attributes: Vec<OtelLogValueNode>,
    resource_nodes: Vec<OtelLogValueNode>,
    scope_attributes: Vec<Vec<OtelLogValueNode>>,
    scope_nodes: Vec<Vec<OtelLogValueNode>>,
    record_attributes: Vec<Vec<OtelLogValueNode>>,
    record_nodes: Vec<Vec<OtelLogValueNode>>,
    scopes: Vec<OtelLogExportScopeView>,
    records: Vec<OtelLogExportRecordView>,
    batch: OtelLogExportBatchView,
}

impl ExportViewStorage {
    /// The batch view handed to the C callback. Valid only while `self` is alive.
    pub(crate) fn view(&self) -> *const OtelLogExportBatchView {
        &self.batch
    }
}

/// Per-record conversion output, held until every vector has settled and pointers can be
/// taken safely.
struct ConvertedRecord {
    view: OtelLogExportRecordView,
    scope_index: usize,
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
}

fn convert_record(record: &SdkLogRecord, scope_index: usize) -> ConversionResult<ConvertedRecord> {
    let mut present_fields = 0u64;

    let mut timestamp_unix_nanos = 0u64;
    if let Some(timestamp) = record.timestamp() {
        timestamp_unix_nanos = timestamp_nanos(timestamp, "timestamp")?;
        present_fields |= OTEL_LOG_EXPORT_FIELD_TIMESTAMP;
    }
    let mut observed_timestamp_unix_nanos = 0u64;
    if let Some(timestamp) = record.observed_timestamp() {
        observed_timestamp_unix_nanos = timestamp_nanos(timestamp, "observed timestamp")?;
        present_fields |= OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP;
    }

    let severity_text = optional_string_view(record.severity_text())?;
    if record.severity_text().is_some() {
        present_fields |= OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT;
    }
    let event_name = optional_string_view(record.event_name())?;
    if record.event_name().is_some() {
        present_fields |= OTEL_LOG_EXPORT_FIELD_EVENT_NAME;
    }
    let target = optional_string_view(record.target().map(|target| target.as_ref()))?;
    if record.target().is_some() {
        present_fields |= OTEL_LOG_EXPORT_FIELD_TARGET;
    }
    if record.body().is_some() {
        present_fields |= OTEL_LOG_EXPORT_FIELD_BODY;
    }

    let mut trace_context = OtelLogTraceContext {
        trace_id: [0; 16],
        span_id: [0; 8],
        trace_flags: 0,
        reserved: [0; 7],
    };
    if let Some(context) = record.trace_context() {
        trace_context.trace_id = context.trace_id.to_bytes();
        trace_context.span_id = context.span_id.to_bytes();
        trace_context.trace_flags = context.trace_flags.map_or(0, |flags| flags.to_u8());
        present_fields |= OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT;
    }

    // Attribute order (and duplicate keys) are preserved exactly as the upstream record
    // appended them.
    let mut attributes: Vec<(&Key, Source<'_>)> = Vec::new();
    let attribute_count = record.attributes_iter().count();
    attributes.try_reserve_exact(attribute_count).map_err(|_| {
        ConversionError::new("failed to allocate the exported log record attribute list")
    })?;
    for (key, value) in record.attributes_iter() {
        attributes.push((key, Source::Any(value)));
    }
    let flattened = Flattener::new().flatten(record.body(), &attributes)?;

    Ok(ConvertedRecord {
        view: OtelLogExportRecordView {
            struct_size: std::mem::size_of::<OtelLogExportRecordView>() as u64,
            present_fields,
            timestamp_unix_nanos,
            observed_timestamp_unix_nanos,
            severity_number: record.severity_number().map_or(0, severity_number),
            reserved_flags: 0,
            severity_text,
            event_name,
            target,
            body: flattened.body,
            attributes: std::ptr::null(),
            attribute_count: flattened.attributes.len(),
            value_nodes: std::ptr::null(),
            value_node_count: flattened.nodes.len(),
            trace_context,
            // Patched once the scope vector has settled; see `convert_batch`.
            scope: std::ptr::null(),
            reserved: [0; 4],
        },
        scope_index,
        attributes: flattened.attributes,
        nodes: flattened.nodes,
    })
}

/// Per-scope conversion output.
struct ConvertedScope {
    view: OtelLogExportScopeView,
    attributes: Vec<OtelLogValueNode>,
    nodes: Vec<OtelLogValueNode>,
}

fn convert_scope(scope: &InstrumentationScope) -> ConversionResult<ConvertedScope> {
    let mut attributes: Vec<(&Key, Source<'_>)> = Vec::new();
    let attribute_count = scope.attributes().count();
    attributes.try_reserve_exact(attribute_count).map_err(|_| {
        ConversionError::new("failed to allocate the exported log scope attribute list")
    })?;
    for attribute in scope.attributes() {
        attributes.push((&attribute.key, Source::Attribute(&attribute.value)));
    }
    let flattened = Flattener::new().flatten(None, &attributes)?;
    Ok(ConvertedScope {
        view: OtelLogExportScopeView {
            struct_size: std::mem::size_of::<OtelLogExportScopeView>() as u64,
            name: string_view(scope.name())?,
            version: optional_string_view(scope.version())?,
            schema_url: optional_string_view(scope.schema_url())?,
            attributes: std::ptr::null(),
            attribute_count: flattened.attributes.len(),
            value_nodes: std::ptr::null(),
            value_node_count: flattened.nodes.len(),
            reserved: [0; 2],
        },
        attributes: flattened.attributes,
        nodes: flattened.nodes,
    })
}

fn try_push<T>(values: &mut Vec<T>, value: T, what: &str) -> ConversionResult<()> {
    values
        .try_reserve(1)
        .map_err(|_| ConversionError::new(format!("failed to allocate an exported log {what}")))?;
    values.push(value);
    Ok(())
}

/// Convert one borrowed upstream batch plus the exporter's resource into the C view.
///
/// The returned storage owns every buffer the view points at, so dropping it invalidates the
/// whole view at once. Conversion is all-or-nothing: a partially converted batch is never
/// presented to a callback.
pub(crate) fn convert_batch(
    batch: &LogBatch<'_>,
    resource: &Resource,
) -> ConversionResult<Box<ExportViewStorage>> {
    let mut resource_attributes: Vec<(&Key, Source<'_>)> = Vec::new();
    let resource_attribute_count = resource.iter().count();
    resource_attributes
        .try_reserve_exact(resource_attribute_count)
        .map_err(|_| {
            ConversionError::new("failed to allocate the exported log resource attribute list")
        })?;
    for (key, value) in resource.iter() {
        resource_attributes.push((key, Source::Attribute(value)));
    }
    let resource_flattened = Flattener::new().flatten(None, &resource_attributes)?;

    let mut scopes: Vec<OtelLogExportScopeView> = Vec::new();
    let mut scope_attributes: Vec<Vec<OtelLogValueNode>> = Vec::new();
    let mut scope_nodes: Vec<Vec<OtelLogValueNode>> = Vec::new();
    // Scopes are deduplicated by pointer identity: the batch borrows each scope, so equal
    // addresses mean the same scope. A missed match only costs one extra converted scope.
    let mut scope_keys: Vec<*const InstrumentationScope> = Vec::new();

    let mut records: Vec<OtelLogExportRecordView> = Vec::new();
    let mut record_scopes: Vec<usize> = Vec::new();
    let mut record_attributes: Vec<Vec<OtelLogValueNode>> = Vec::new();
    let mut record_nodes: Vec<Vec<OtelLogValueNode>> = Vec::new();

    let mut record_count = 0usize;
    for (record, scope) in batch.iter() {
        record_count = record_count.checked_add(1).ok_or_else(|| {
            ConversionError::new("exported log batch record count overflowed the address space")
        })?;
        if record_count > OTEL_LOG_EXPORT_MAX_RECORDS {
            return Err(ConversionError::new(format!(
                "exported log batch carries more than {OTEL_LOG_EXPORT_MAX_RECORDS} records"
            )));
        }
        let key: *const InstrumentationScope = scope;
        let scope_index = match scope_keys.iter().position(|candidate| *candidate == key) {
            Some(index) => index,
            None => {
                let converted = convert_scope(scope)?;
                try_push(&mut scopes, converted.view, "scope view")?;
                try_push(&mut scope_keys, key, "scope key")?;
                try_push(
                    &mut scope_attributes,
                    converted.attributes,
                    "scope attribute array",
                )?;
                try_push(&mut scope_nodes, converted.nodes, "scope node pool")?;
                scopes.len() - 1
            }
        };
        let converted = convert_record(record, scope_index)?;
        try_push(
            &mut record_scopes,
            converted.scope_index,
            "record scope index",
        )?;
        try_push(&mut records, converted.view, "record view")?;
        try_push(
            &mut record_attributes,
            converted.attributes,
            "record attribute array",
        )?;
        try_push(&mut record_nodes, converted.nodes, "record node pool")?;
    }

    let mut storage = Box::new(ExportViewStorage {
        resource_attributes: resource_flattened.attributes,
        resource_nodes: resource_flattened.nodes,
        scope_attributes,
        scope_nodes,
        record_attributes,
        record_nodes,
        scopes,
        records,
        batch: OtelLogExportBatchView {
            struct_size: std::mem::size_of::<OtelLogExportBatchView>() as u64,
            records: std::ptr::null(),
            record_count: 0,
            resource_schema_url: optional_string_view(resource.schema_url())?,
            resource_attributes: std::ptr::null(),
            resource_attribute_count: 0,
            resource_value_nodes: std::ptr::null(),
            resource_value_node_count: 0,
            reserved: [0; 4],
        },
    });

    // Every vector has reached its final length; only now are interior pointers taken.
    for (index, scope) in storage.scopes.iter_mut().enumerate() {
        scope.attributes = storage.scope_attributes[index].as_ptr();
        scope.value_nodes = storage.scope_nodes[index].as_ptr();
    }
    let scopes_base: *const OtelLogExportScopeView = storage.scopes.as_ptr();
    let scope_count = storage.scopes.len();
    for (index, record) in storage.records.iter_mut().enumerate() {
        record.attributes = storage.record_attributes[index].as_ptr();
        record.value_nodes = storage.record_nodes[index].as_ptr();
        let scope_index = record_scopes[index];
        debug_assert!(scope_index < scope_count);
        // SAFETY: `convert_record` stored an index that `convert_batch` produced from
        // `storage.scopes`, which is complete and no longer mutated.
        record.scope = unsafe { scopes_base.add(scope_index) };
    }
    storage.batch.records = storage.records.as_ptr();
    storage.batch.record_count = storage.records.len();
    storage.batch.resource_attributes = storage.resource_attributes.as_ptr();
    storage.batch.resource_attribute_count = storage.resource_attributes.len();
    storage.batch.resource_value_nodes = storage.resource_nodes.as_ptr();
    storage.batch.resource_value_node_count = storage.resource_nodes.len();
    Ok(storage)
}
