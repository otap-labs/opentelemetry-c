//! SDK implementation of the internal Logs vtable.
//!
//! This is where a borrowed `otel_log_record_view_t` is validated and turned into an owned
//! pinned-Rust `SdkLogRecord`. Nothing borrowed from the caller escapes `logger_emit`.
//!
//! ## Why the record is converted in two passes
//!
//! Structured bodies and attribute values arrive as a *flat node pool* addressed by
//! `[first, first + count)` ranges. The first pass is purely structural and allocates
//! nothing: it checks tags, ranges, limits, key rules, and single-ownership of every pool
//! node, and computes each node's subtree depth. The second pass converts bottom-up, from
//! the highest index down to zero.
//!
//! Two invariants make that possible and make the conversion provably terminating without
//! recursion or a visited set:
//!
//! * a container's children must live at a **strictly greater** pool index than the
//!   container itself, so a cycle cannot be expressed at all; and
//! * every pool node must be referenced **exactly once**, by one container or by one root,
//!   so bottom-up conversion can move each converted child into its parent without cloning,
//!   and no unreachable node is ever converted.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry::{InstrumentationScope, Key, SpanId, TraceFlags, TraceId};
use opentelemetry_c_abi::{
    log_severity_is_valid, OtelBool, OtelLogRecordView, OtelLogValue, OtelLogValueNode,
    OtelLogValueType, OtelLogsVtable, OtelScopeConfig, OtelStatus, OTEL_LOGS_IMPL_ABI_VERSION,
    OTEL_LOG_FIELD_KNOWN_MASK, OTEL_LOG_FIELD_OBSERVED_TIMESTAMP, OTEL_LOG_FIELD_TIMESTAMP,
    OTEL_LOG_FIELD_TRACE_CONTEXT, OTEL_LOG_MAX_ARRAY_ELEMENTS, OTEL_LOG_MAX_ATTRIBUTES,
    OTEL_LOG_MAX_BYTES_LEN, OTEL_LOG_MAX_MAP_ENTRIES, OTEL_LOG_MAX_STRING_LEN,
    OTEL_LOG_MAX_VALUE_DEPTH, OTEL_LOG_MAX_VALUE_NODES, OTEL_LOG_TRACE_FLAGS_SUPPORTED_MASK,
};
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};

use crate::error::{fail, fail_abi, fail_owned};

fn guard_ptr(f: impl FnOnce() -> *mut c_void) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(std::ptr::null_mut())
}

fn guard_status(f: impl FnOnce() -> OtelStatus) -> OtelStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(OtelStatus::InternalError)
}

fn guard_unit(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

fn guard_bool(f: impl FnOnce() -> OtelBool) -> OtelBool {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(0)
}

/// Map a validated C severity number onto the pinned `Severity` enum.
///
/// Written as an explicit match rather than a transmute so that a change to the pinned
/// enum's discriminants becomes a compile error instead of silent corruption.
fn severity_from_u32(severity: u32) -> Option<Severity> {
    Some(match severity {
        1 => Severity::Trace,
        2 => Severity::Trace2,
        3 => Severity::Trace3,
        4 => Severity::Trace4,
        5 => Severity::Debug,
        6 => Severity::Debug2,
        7 => Severity::Debug3,
        8 => Severity::Debug4,
        9 => Severity::Info,
        10 => Severity::Info2,
        11 => Severity::Info3,
        12 => Severity::Info4,
        13 => Severity::Warn,
        14 => Severity::Warn2,
        15 => Severity::Warn3,
        16 => Severity::Warn4,
        17 => Severity::Error,
        18 => Severity::Error2,
        19 => Severity::Error3,
        20 => Severity::Error4,
        21 => Severity::Fatal,
        22 => Severity::Fatal2,
        23 => Severity::Fatal3,
        24 => Severity::Fatal4,
        _ => return None,
    })
}

// ---- Provider / logger contexts ---------------------------------------------

fn build_logger(ctx: *mut c_void, scope: &OtelScopeConfig) -> *mut c_void {
    if ctx.is_null() {
        fail(OtelStatus::InvalidArgument, "logs provider context is NULL");
        return std::ptr::null_mut();
    }
    // SAFETY: `ctx` is a live provider context produced by this crate.
    let provider = unsafe { &*(ctx as *const SdkLoggerProvider) };
    let name = match unsafe { scope.name.to_string_strict() } {
        Ok(name) => name,
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    };
    let mut builder = InstrumentationScope::builder(name);
    match unsafe { scope.version.to_string_strict() } {
        Ok(version) if !version.is_empty() => builder = builder.with_version(version),
        Ok(_) => {}
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    }
    match unsafe { scope.schema_url.to_string_strict() } {
        Ok(schema_url) if !schema_url.is_empty() => builder = builder.with_schema_url(schema_url),
        Ok(_) => {}
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    }
    let attributes = match unsafe {
        crate::vtable::collect_unique_key_values(scope.attributes, scope.attribute_count)
    } {
        Ok(attributes) => attributes,
        Err(_) => return std::ptr::null_mut(),
    };
    if !attributes.is_empty() {
        builder = builder.with_attributes(attributes);
    }
    Box::into_raw(Box::new(provider.logger_with_scope(builder.build()))) as *mut c_void
}

extern "C" fn provider_get_logger(
    provider_ctx: *mut c_void,
    scope: *const OtelScopeConfig,
) -> *mut c_void {
    guard_ptr(|| {
        if scope.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "logs scope configuration is NULL",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: the API guarantees a readable scope for the duration of this call.
        build_logger(provider_ctx, unsafe { &*scope })
    })
}

extern "C" fn provider_retain(provider_ctx: *mut c_void) -> *mut c_void {
    guard_ptr(|| {
        if provider_ctx.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: live provider context produced by this crate. `SdkLoggerProvider` is an
        // `Arc` newtype, so cloning yields an independent owned reference.
        let provider = unsafe { &*(provider_ctx as *const SdkLoggerProvider) };
        Box::into_raw(Box::new(provider.clone())) as *mut c_void
    })
}

extern "C" fn provider_free(provider_ctx: *mut c_void) {
    guard_unit(|| {
        if !provider_ctx.is_null() {
            // SAFETY: exactly one owned reference per context, released once.
            drop(unsafe { Box::from_raw(provider_ctx as *mut SdkLoggerProvider) });
        }
    });
}

extern "C" fn logger_free(logger_ctx: *mut c_void) {
    guard_unit(|| {
        if !logger_ctx.is_null() {
            // SAFETY: logger contexts are single-owner.
            drop(unsafe { Box::from_raw(logger_ctx as *mut SdkLogger) });
        }
    });
}

extern "C" fn logger_enabled(logger_ctx: *mut c_void, severity: u32) -> OtelBool {
    guard_bool(|| {
        if logger_ctx.is_null() {
            return 0;
        }
        let Some(severity) = severity_from_u32(severity) else {
            return 0;
        };
        // SAFETY: live logger context produced by this crate.
        let logger = unsafe { &*(logger_ctx as *const SdkLogger) };
        // The pinned `event_enabled` requires a target; the C API exposes no target, and
        // overriding it would rewrite the caller's instrumentation scope name in OTLP, so an
        // empty target is passed deliberately.
        OtelBool::from(logger.event_enabled(severity, "", None))
    })
}

// ---- Record conversion -------------------------------------------------------

/// Per-node structural facts computed by the validation pass.
#[derive(Clone, Copy)]
struct NodeInfo {
    /// Height of the subtree rooted at this node; a scalar has depth 1.
    depth: u32,
    /// Whether some container or root already claimed this node.
    referenced: bool,
}

struct Validator<'a> {
    nodes: &'a [OtelLogValueNode],
    info: Vec<NodeInfo>,
}

/// Where a value sits, which decides whether its key must be present or absent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeRole {
    ArrayElement,
    MapEntry,
}

fn try_vec<T: Clone>(value: T, len: usize) -> Result<Vec<T>, OtelStatus> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| {
        fail(
            OtelStatus::InternalError,
            "failed to allocate log record conversion state",
        )
    })?;
    out.resize(len, value);
    Ok(out)
}

impl<'a> Validator<'a> {
    fn new(nodes: &'a [OtelLogValueNode]) -> Result<Self, OtelStatus> {
        let info = try_vec(
            NodeInfo {
                depth: 1,
                referenced: false,
            },
            nodes.len(),
        )?;
        Ok(Self { nodes, info })
    }

    /// Validate one value's own fields (tag, reserved word, scalar payload limits).
    ///
    /// Container ranges are validated separately because the rules differ between a root
    /// value and a pool node.
    fn check_value_scalar(&self, value: &OtelLogValue) -> Result<OtelLogValueType, OtelStatus> {
        if value.reserved != 0 {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "log value reserved field must be zero",
            ));
        }
        let Some(tag) = OtelLogValueType::from_u32(value.value_type) else {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "log value has an unknown value_type tag",
            ));
        };
        match tag {
            // The pinned `AnyValue` has no null/absent variant, so "empty" is only
            // meaningful as "this record has no body". Anywhere else it would have to be
            // silently substituted with some other value, which would misreport data.
            OtelLogValueType::Empty => {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "an empty log value is only valid as an absent record body",
                ))
            }
            OtelLogValueType::String => {
                // SAFETY: the ABI contract guarantees the view is readable for this call.
                let s = unsafe { value.value.string_value.as_str() }.map_err(fail_abi)?;
                if s.len() > OTEL_LOG_MAX_STRING_LEN {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "log string value exceeds the maximum supported length",
                    ));
                }
            }
            OtelLogValueType::Bytes => {
                // SAFETY: as above.
                let b = unsafe { value.value.bytes_value.as_slice() }.map_err(fail_abi)?;
                if b.len() > OTEL_LOG_MAX_BYTES_LEN {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "log bytes value exceeds the maximum supported length",
                    ));
                }
            }
            _ => {}
        }
        Ok(tag)
    }

    /// Claim `[first, first + count)` for a container, enforcing the range, the per-container
    /// element budget, single ownership, and (for pool nodes) strict forward addressing.
    ///
    /// `owner` is `None` for a root value, which may address any part of the pool.
    fn claim_children(
        &mut self,
        tag: OtelLogValueType,
        value: &OtelLogValue,
        owner: Option<usize>,
    ) -> Result<std::ops::Range<usize>, OtelStatus> {
        // SAFETY: `children` is the active union member for Array/Map, checked by the caller.
        let range = unsafe { value.value.children };
        let first = range.first as usize;
        let count = range.count as usize;
        let limit = if tag == OtelLogValueType::Map {
            OTEL_LOG_MAX_MAP_ENTRIES
        } else {
            OTEL_LOG_MAX_ARRAY_ELEMENTS
        };
        if count > limit {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "log container exceeds the maximum supported element count",
            ));
        }
        if count == 0 {
            return Ok(0..0);
        }
        let end = first.checked_add(count).ok_or_else(|| {
            fail(
                OtelStatus::InvalidArgument,
                "log container child range overflows",
            )
        })?;
        if end > self.nodes.len() {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "log container child range is outside the node pool",
            ));
        }
        if let Some(owner) = owner {
            // Strictly forward addressing is what makes cycles unrepresentable.
            if first <= owner {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "log container children must live at a strictly greater pool index",
                ));
            }
        }
        for index in first..end {
            if self.info[index].referenced {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "log value node is referenced more than once",
                ));
            }
            self.info[index].referenced = true;
        }
        Ok(first..end)
    }

    /// Validate the key of a pool node against its parent's kind, returning it for
    /// duplicate detection.
    fn check_child_key(&self, index: usize, role: NodeRole) -> Result<Option<&'a str>, OtelStatus> {
        let nodes: &'a [OtelLogValueNode] = self.nodes;
        // SAFETY: the ABI contract guarantees the view is readable for this call.
        let key = unsafe { nodes[index].key.as_str() }.map_err(fail_abi)?;
        match role {
            NodeRole::ArrayElement => {
                if !key.is_empty() {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "array element nodes must have an empty key",
                    ));
                }
                Ok(None)
            }
            NodeRole::MapEntry => {
                if key.is_empty() {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "map entry nodes must have a non-empty key",
                    ));
                }
                if key.len() > OTEL_LOG_MAX_STRING_LEN {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "map entry key exceeds the maximum supported length",
                    ));
                }
                Ok(Some(key))
            }
        }
    }

    /// Structural pass over the whole pool plus the record's roots.
    ///
    /// Nodes are visited in ascending index order, so a container is always seen before its
    /// (strictly greater) children. Depth then propagates in a descending pass.
    fn validate(&mut self, roots: &[&OtelLogValue]) -> Result<(), OtelStatus> {
        // Ascending: tags, payload limits, and child-range claims.
        for index in 0..self.nodes.len() {
            let value = self.nodes[index].value;
            let tag = self.check_value_scalar(&value)?;
            if matches!(tag, OtelLogValueType::Array | OtelLogValueType::Map) {
                let range = self.claim_children(tag, &value, Some(index))?;
                self.check_children(tag, range)?;
            }
        }
        for root in roots {
            let tag = self.check_value_scalar(root)?;
            if matches!(tag, OtelLogValueType::Array | OtelLogValueType::Map) {
                let range = self.claim_children(tag, root, None)?;
                self.check_children(tag, range)?;
            }
        }
        // Every pool node must belong to exactly one parent, so nothing unreachable is
        // converted and no node is shared between two parents.
        if let Some(index) = self.info.iter().position(|info| !info.referenced) {
            return Err(fail_owned(
                OtelStatus::InvalidArgument,
                format!("log value node {index} is not referenced by any container or root"),
            ));
        }
        // Descending: subtree depth. Children always have a greater index, so their depth is
        // final by the time their parent is visited.
        for index in (0..self.nodes.len()).rev() {
            let value = self.nodes[index].value;
            if matches!(
                OtelLogValueType::from_u32(value.value_type),
                Some(OtelLogValueType::Array) | Some(OtelLogValueType::Map)
            ) {
                // SAFETY: validated as a container above.
                let range = unsafe { value.value.children };
                let mut child_depth = 0;
                for child in range.first as usize..(range.first as usize + range.count as usize) {
                    child_depth = child_depth.max(self.info[child].depth);
                }
                self.info[index].depth = child_depth + 1;
            }
        }
        for root in roots {
            let depth = self.root_depth(root);
            if depth as usize > OTEL_LOG_MAX_VALUE_DEPTH {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "log value nesting exceeds the maximum supported depth",
                ));
            }
        }
        Ok(())
    }

    fn root_depth(&self, root: &OtelLogValue) -> u32 {
        if !matches!(
            OtelLogValueType::from_u32(root.value_type),
            Some(OtelLogValueType::Array) | Some(OtelLogValueType::Map)
        ) {
            return 1;
        }
        // SAFETY: validated as a container.
        let range = unsafe { root.value.children };
        let mut child_depth = 0;
        for child in range.first as usize..(range.first as usize + range.count as usize) {
            child_depth = child_depth.max(self.info[child].depth);
        }
        child_depth + 1
    }

    /// Key rules and, for maps, duplicate detection over a claimed child range.
    fn check_children(
        &self,
        tag: OtelLogValueType,
        range: std::ops::Range<usize>,
    ) -> Result<(), OtelStatus> {
        let role = if tag == OtelLogValueType::Map {
            NodeRole::MapEntry
        } else {
            NodeRole::ArrayElement
        };
        let mut seen: Vec<&str> = Vec::new();
        if role == NodeRole::MapEntry {
            seen.try_reserve_exact(range.len()).map_err(|_| {
                fail(
                    OtelStatus::InternalError,
                    "failed to allocate map key validation state",
                )
            })?;
        }
        for index in range {
            if let Some(key) = self.check_child_key(index, role)? {
                // The pinned `AnyValue::Map` is a `HashMap`, which would silently drop one
                // of a duplicate pair. Reject instead of losing data.
                if seen.contains(&key) {
                    return Err(fail(
                        OtelStatus::InvalidArgument,
                        "duplicate key in a log map value",
                    ));
                }
                seen.push(key);
            }
        }
        Ok(())
    }
}

/// Bottom-up conversion. Requires a successful [`Validator::validate`].
struct Converter<'a> {
    nodes: &'a [OtelLogValueNode],
    converted: Vec<Option<AnyValue>>,
}

impl<'a> Converter<'a> {
    fn new(nodes: &'a [OtelLogValueNode]) -> Result<Self, OtelStatus> {
        let converted = try_vec(None, nodes.len())?;
        Ok(Self { nodes, converted })
    }

    /// Convert every pool node, highest index first, so a container's children are already
    /// owned values that can be moved into it.
    fn convert_pool(&mut self) -> Result<(), OtelStatus> {
        for index in (0..self.nodes.len()).rev() {
            let value = self.convert_value(&self.nodes[index].value)?;
            self.converted[index] = Some(value);
        }
        Ok(())
    }

    fn take_child(&mut self, index: usize) -> Result<AnyValue, OtelStatus> {
        self.converted[index].take().ok_or_else(|| {
            fail(
                OtelStatus::InternalError,
                "log value node was consumed twice",
            )
        })
    }

    fn convert_value(&mut self, value: &OtelLogValue) -> Result<AnyValue, OtelStatus> {
        // Validation already accepted the tag and the payload.
        let tag = OtelLogValueType::from_u32(value.value_type).ok_or_else(|| {
            fail(
                OtelStatus::InternalError,
                "log value tag changed during conversion",
            )
        })?;
        Ok(match tag {
            // Validation rejects `Empty` everywhere it could reach conversion.
            OtelLogValueType::Empty => {
                return Err(fail(
                    OtelStatus::InternalError,
                    "an empty log value reached conversion",
                ))
            }
            OtelLogValueType::String => {
                // SAFETY: readable for this call, already validated as UTF-8.
                let s = unsafe { value.value.string_value.to_string_strict() }.map_err(fail_abi)?;
                AnyValue::String(s.into())
            }
            // SAFETY (each arm): the union member matches the validated tag.
            OtelLogValueType::Bool => AnyValue::Boolean(unsafe { value.value.bool_value } != 0),
            OtelLogValueType::Int64 => AnyValue::Int(unsafe { value.value.int64_value }),
            OtelLogValueType::Double => AnyValue::Double(unsafe { value.value.double_value }),
            OtelLogValueType::Bytes => {
                let bytes = unsafe { value.value.bytes_value.as_slice() }.map_err(fail_abi)?;
                let mut owned: Vec<u8> = Vec::new();
                owned.try_reserve_exact(bytes.len()).map_err(|_| {
                    fail(
                        OtelStatus::InternalError,
                        "failed to allocate log bytes value",
                    )
                })?;
                owned.extend_from_slice(bytes);
                AnyValue::Bytes(Box::new(owned))
            }
            OtelLogValueType::Array => {
                let range = unsafe { value.value.children };
                let mut items: Vec<AnyValue> = Vec::new();
                items.try_reserve_exact(range.count as usize).map_err(|_| {
                    fail(
                        OtelStatus::InternalError,
                        "failed to allocate log array value",
                    )
                })?;
                for child in range.first as usize..(range.first as usize + range.count as usize) {
                    items.push(self.take_child(child)?);
                }
                AnyValue::ListAny(Box::new(items))
            }
            OtelLogValueType::Map => {
                let range = unsafe { value.value.children };
                let mut map = std::collections::HashMap::new();
                map.try_reserve(range.count as usize).map_err(|_| {
                    fail(
                        OtelStatus::InternalError,
                        "failed to allocate log map value",
                    )
                })?;
                for child in range.first as usize..(range.first as usize + range.count as usize) {
                    // SAFETY: readable for this call, already validated as non-empty UTF-8.
                    let key =
                        unsafe { self.nodes[child].key.to_string_strict() }.map_err(fail_abi)?;
                    let converted = self.take_child(child)?;
                    map.insert(Key::from(key), converted);
                }
                debug_assert_eq!(map.len(), range.count as usize);
                AnyValue::Map(Box::new(map))
            }
        })
    }
}

fn system_time_from_unix_nanos(nanos: u64, what: &str) -> Result<SystemTime, OtelStatus> {
    UNIX_EPOCH
        .checked_add(Duration::from_nanos(nanos))
        .ok_or_else(|| {
            fail_owned(
                OtelStatus::InvalidArgument,
                format!("{what} is not representable as a system time"),
            )
        })
}

fn emit_record(logger: &SdkLogger, view: &OtelLogRecordView) -> OtelStatus {
    if view.reserved_flags != 0
        || view.body.reserved != 0
        || view.reserved.iter().any(|word| *word != 0)
    {
        return fail(
            OtelStatus::InvalidArgument,
            "log record reserved fields must be zero",
        );
    }
    if view.present_fields & !OTEL_LOG_FIELD_KNOWN_MASK != 0 {
        return fail(
            OtelStatus::InvalidConfig,
            "log record present_fields contains bits unknown to this library version",
        );
    }
    if !log_severity_is_valid(view.severity_number) {
        return fail(
            OtelStatus::InvalidArgument,
            "log record severity_number must be 0 (absent) or 1..=24",
        );
    }
    if view.attribute_count > OTEL_LOG_MAX_ATTRIBUTES {
        return fail(
            OtelStatus::InvalidArgument,
            "log record exceeds the maximum supported attribute count",
        );
    }
    if view.value_node_count > OTEL_LOG_MAX_VALUE_NODES {
        return fail(
            OtelStatus::InvalidArgument,
            "log record exceeds the maximum supported value node count",
        );
    }
    if view.attribute_count > 0 && view.attributes.is_null() {
        return fail(
            OtelStatus::InvalidArgument,
            "log record attribute_count is non-zero but attributes is NULL",
        );
    }
    if view.value_node_count > 0 && view.value_nodes.is_null() {
        return fail(
            OtelStatus::InvalidArgument,
            "log record value_node_count is non-zero but value_nodes is NULL",
        );
    }

    // SAFETY: counts are bounded well below `isize::MAX` and the pointers are non-NULL
    // whenever the count is non-zero; the API guarantees readability for this call.
    let attributes: &[OtelLogValueNode] = if view.attribute_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(view.attributes, view.attribute_count) }
    };
    let nodes: &[OtelLogValueNode] = if view.value_node_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(view.value_nodes, view.value_node_count) }
    };

    let body_present = view.body.value_type != OtelLogValueType::Empty as u32;
    let mut roots: Vec<&OtelLogValue> = Vec::new();
    if roots
        .try_reserve_exact(attributes.len() + usize::from(body_present))
        .is_err()
    {
        return fail(
            OtelStatus::InternalError,
            "failed to allocate log record root list",
        );
    }
    if body_present {
        roots.push(&view.body);
    }
    let mut attribute_keys: Vec<&str> = Vec::new();
    if attribute_keys.try_reserve_exact(attributes.len()).is_err() {
        return fail(
            OtelStatus::InternalError,
            "failed to allocate log attribute key list",
        );
    }
    for attribute in attributes {
        // SAFETY: readable for this call per the ABI contract.
        let key = match unsafe { attribute.key.as_str() } {
            Ok(key) => key,
            Err(err) => return fail_abi(err),
        };
        if key.is_empty() {
            return fail(
                OtelStatus::InvalidArgument,
                "log record attribute keys must not be empty",
            );
        }
        if key.len() > OTEL_LOG_MAX_STRING_LEN {
            return fail(
                OtelStatus::InvalidArgument,
                "log record attribute key exceeds the maximum supported length",
            );
        }
        attribute_keys.push(key);
        roots.push(&attribute.value);
    }

    // Structural validation first: nothing is allocated for the caller's content, and no
    // partially converted record can be handed to a processor.
    let mut validator = match Validator::new(nodes) {
        Ok(validator) => validator,
        Err(status) => return status,
    };
    if let Err(status) = validator.validate(&roots) {
        return status;
    }

    let mut converter = match Converter::new(nodes) {
        Ok(converter) => converter,
        Err(status) => return status,
    };
    if let Err(status) = converter.convert_pool() {
        return status;
    }

    let mut record = logger.create_log_record();
    if view.present_fields & OTEL_LOG_FIELD_TIMESTAMP != 0 {
        match system_time_from_unix_nanos(view.timestamp_unix_nanos, "log record timestamp") {
            Ok(time) => record.set_timestamp(time),
            Err(status) => return status,
        }
    }
    if view.present_fields & OTEL_LOG_FIELD_OBSERVED_TIMESTAMP != 0 {
        match system_time_from_unix_nanos(
            view.observed_timestamp_unix_nanos,
            "log record observed timestamp",
        ) {
            Ok(time) => record.set_observed_timestamp(time),
            Err(status) => return status,
        }
    }
    if view.severity_number != 0 {
        let Some(severity) = severity_from_u32(view.severity_number) else {
            return fail(
                OtelStatus::InvalidArgument,
                "log record severity_number must be 0 (absent) or 1..=24",
            );
        };
        record.set_severity_number(severity);
        // The pinned `set_severity_text` takes `&'static str`, so the only text that can be
        // set is the canonical name derived from the number. A caller-supplied severity text
        // is therefore not expressible in this version.
        record.set_severity_text(severity.name());
    }
    if view.present_fields & OTEL_LOG_FIELD_TRACE_CONTEXT != 0 {
        let ctx = &view.trace_context;
        if ctx.reserved.iter().any(|byte| *byte != 0) {
            return fail(
                OtelStatus::InvalidArgument,
                "log record trace context reserved bytes must be zero",
            );
        }
        if ctx.trace_flags & !OTEL_LOG_TRACE_FLAGS_SUPPORTED_MASK != 0 {
            return fail(
                OtelStatus::InvalidConfig,
                "log record trace context uses trace flags unknown to this library version",
            );
        }
        if ctx.trace_id == [0u8; 16] {
            return fail(
                OtelStatus::InvalidArgument,
                "log record trace context trace_id must not be all zero",
            );
        }
        if ctx.span_id == [0u8; 8] {
            return fail(
                OtelStatus::InvalidArgument,
                "log record trace context span_id must not be all zero",
            );
        }
        // Set explicitly so the pinned SDK never substitutes the ambient Rust `Context`,
        // which a C caller has no way to observe or control.
        record.set_trace_context(
            TraceId::from_bytes(ctx.trace_id),
            SpanId::from_bytes(ctx.span_id),
            Some(TraceFlags::new(ctx.trace_flags)),
        );
    }
    if body_present {
        match converter.convert_value(&view.body) {
            Ok(body) => record.set_body(body),
            Err(status) => return status,
        }
    }
    for (key, attribute) in attribute_keys.iter().zip(attributes.iter()) {
        let value = match converter.convert_value(&attribute.value) {
            Ok(value) => value,
            Err(status) => return status,
        };
        // Duplicate attribute keys are preserved rather than rejected: the pinned SDK appends
        // without deduplicating, and deduplicating here would add a per-emit hash set.
        record.add_attribute(Key::from((*key).to_owned()), value);
    }

    logger.emit(record);
    OtelStatus::Ok
}

extern "C" fn logger_emit(logger_ctx: *mut c_void, record: *const OtelLogRecordView) -> OtelStatus {
    guard_status(|| {
        if logger_ctx.is_null() {
            return fail(OtelStatus::InvalidArgument, "logger context is NULL");
        }
        if record.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view must not be NULL",
            );
        }
        // Re-check the size prefix: this vtable can be driven by any API build, so it must
        // not rely on the caller having validated anything.
        // SAFETY: the ABI guarantees at least the leading `struct_size` word is readable.
        let struct_size = unsafe { record.cast::<u64>().read() };
        if struct_size < std::mem::size_of::<OtelLogRecordView>() as u64 {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view struct_size is smaller than the supported prefix",
            );
        }
        // SAFETY: `struct_size` covers at least the fields read below.
        let view = unsafe { &*record };
        // SAFETY: live logger context produced by this crate.
        let logger = unsafe { &*(logger_ctx as *const SdkLogger) };
        emit_record(logger, view)
    })
}

pub(crate) static SDK_LOGS_VTABLE: OtelLogsVtable = OtelLogsVtable {
    abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelLogsVtable>(),
    provider_get_logger,
    provider_retain,
    provider_free,
    logger_enabled,
    logger_emit,
    logger_free,
};

pub(crate) fn vtable_ptr() -> *const OtelLogsVtable {
    &SDK_LOGS_VTABLE
}

pub(crate) fn provider_ctx(provider: SdkLoggerProvider) -> *mut c_void {
    Box::into_raw(Box::new(provider)) as *mut c_void
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_char;

    use opentelemetry_c_abi::{
        OtelLogBytesView, OtelLogTraceContext, OtelLogValuePayload, OtelLogValueRange,
        OtelStringView,
    };
    use opentelemetry_sdk::logs::in_memory_exporter::LogDataWithResource;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider, SimpleLogProcessor};

    fn sv(value: &str) -> OtelStringView {
        OtelStringView {
            ptr: value.as_ptr().cast::<c_char>(),
            len: value.len(),
        }
    }

    fn value(tag: OtelLogValueType, payload: OtelLogValuePayload) -> OtelLogValue {
        OtelLogValue {
            value_type: tag as u32,
            reserved: 0,
            value: payload,
        }
    }

    fn v_string(s: &str) -> OtelLogValue {
        value(
            OtelLogValueType::String,
            OtelLogValuePayload {
                string_value: sv(s),
            },
        )
    }

    fn v_int(i: i64) -> OtelLogValue {
        value(
            OtelLogValueType::Int64,
            OtelLogValuePayload { int64_value: i },
        )
    }

    fn v_bool(b: bool) -> OtelLogValue {
        value(
            OtelLogValueType::Bool,
            OtelLogValuePayload {
                bool_value: u32::from(b),
            },
        )
    }

    fn v_double(d: f64) -> OtelLogValue {
        value(
            OtelLogValueType::Double,
            OtelLogValuePayload { double_value: d },
        )
    }

    fn v_bytes(b: &[u8]) -> OtelLogValue {
        value(
            OtelLogValueType::Bytes,
            OtelLogValuePayload {
                bytes_value: OtelLogBytesView {
                    ptr: b.as_ptr(),
                    len: b.len(),
                },
            },
        )
    }

    fn v_container(tag: OtelLogValueType, first: u32, count: u32) -> OtelLogValue {
        value(
            tag,
            OtelLogValuePayload {
                children: OtelLogValueRange { first, count },
            },
        )
    }

    fn v_empty() -> OtelLogValue {
        value(
            OtelLogValueType::Empty,
            OtelLogValuePayload {
                string_value: OtelStringView::empty(),
            },
        )
    }

    fn element(value: OtelLogValue) -> OtelLogValueNode {
        OtelLogValueNode {
            key: OtelStringView::empty(),
            value,
        }
    }

    fn entry(key: &str, value: OtelLogValue) -> OtelLogValueNode {
        OtelLogValueNode {
            key: sv(key),
            value,
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
            body: v_empty(),
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

    struct Harness {
        exporter: InMemoryLogExporter,
        provider_ctx: *mut c_void,
        logger_ctx: *mut c_void,
    }

    impl Harness {
        fn new() -> Self {
            let exporter = InMemoryLogExporter::default();
            let provider = SdkLoggerProvider::builder()
                .with_log_processor(SimpleLogProcessor::new(exporter.clone()))
                .build();
            let provider_ctx = provider_ctx(provider);
            let scope = OtelScopeConfig {
                name: sv("scope"),
                version: sv("1.2.3"),
                schema_url: OtelStringView::empty(),
                attributes: std::ptr::null(),
                attribute_count: 0,
            };
            let logger_ctx = provider_get_logger(provider_ctx, &scope);
            assert!(!logger_ctx.is_null());
            Self {
                exporter,
                provider_ctx,
                logger_ctx,
            }
        }

        fn emit(&self, view: &OtelLogRecordView) -> OtelStatus {
            logger_emit(self.logger_ctx, view)
        }

        fn emitted(&self) -> Vec<LogDataWithResource> {
            self.exporter.get_emitted_logs().expect("emitted logs")
        }

        fn only(&self) -> LogDataWithResource {
            let mut logs = self.emitted();
            assert_eq!(logs.len(), 1, "expected exactly one emitted record");
            logs.pop().unwrap()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            logger_free(self.logger_ctx);
            provider_free(self.provider_ctx);
        }
    }

    #[test]
    fn scope_is_forwarded_verbatim() {
        let harness = Harness::new();
        let view = record();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        assert_eq!(log.instrumentation.name(), "scope");
        assert_eq!(log.instrumentation.version(), Some("1.2.3"));
        assert_eq!(log.instrumentation.schema_url(), None);
        // No target is exposed, so the scope name must survive into export unchanged.
        assert!(log.record.body().is_none());
        assert!(log.record.severity_number().is_none());
        assert!(log.record.severity_text().is_none());
        // `event_name` is not expressible in this version.
        assert!(log.record.event_name().is_none());
    }

    #[test]
    fn every_severity_gets_its_canonical_text() {
        for number in 1..=24u32 {
            let harness = Harness::new();
            let mut view = record();
            view.severity_number = number;
            assert_eq!(harness.emit(&view), OtelStatus::Ok);
            let log = harness.only();
            let severity = severity_from_u32(number).expect("mapped severity");
            assert_eq!(log.record.severity_number(), Some(severity));
            assert_eq!(log.record.severity_text(), Some(severity.name()));
        }
    }

    #[test]
    fn severity_zero_sets_neither_number_nor_text() {
        let harness = Harness::new();
        let mut view = record();
        view.severity_number = 0;
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        assert!(log.record.severity_number().is_none());
        assert!(log.record.severity_text().is_none());
    }

    #[test]
    fn out_of_range_severity_is_rejected_and_nothing_is_emitted() {
        let harness = Harness::new();
        for number in [25u32, 100, u32::MAX] {
            let mut view = record();
            view.severity_number = number;
            assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        }
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn timestamps_round_trip_and_observed_defaults_when_absent() {
        let harness = Harness::new();
        let mut view = record();
        view.present_fields = OTEL_LOG_FIELD_TIMESTAMP;
        view.timestamp_unix_nanos = 1_700_000_000_123_456_789;
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        assert_eq!(
            log.record.timestamp(),
            Some(UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789))
        );
        // The pinned `SdkLogger::emit` fills the observed timestamp when the record omits it.
        assert!(log.record.observed_timestamp().is_some());
    }

    #[test]
    fn explicit_observed_timestamp_is_preserved() {
        let harness = Harness::new();
        let mut view = record();
        view.present_fields = OTEL_LOG_FIELD_OBSERVED_TIMESTAMP;
        view.observed_timestamp_unix_nanos = 42;
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        assert_eq!(
            log.record.observed_timestamp(),
            Some(UNIX_EPOCH + Duration::from_nanos(42))
        );
        assert!(log.record.timestamp().is_none());
    }

    #[test]
    fn timestamp_zero_is_a_real_instant_only_when_its_presence_bit_is_set() {
        let harness = Harness::new();
        let mut view = record();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        assert!(harness.only().record.timestamp().is_none());

        view.present_fields = OTEL_LOG_FIELD_TIMESTAMP;
        view.timestamp_unix_nanos = 0;
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        assert_eq!(
            harness.emitted().last().unwrap().record.timestamp(),
            Some(UNIX_EPOCH)
        );
    }

    #[test]
    fn unknown_presence_bits_and_non_zero_reserved_fields_are_rejected() {
        let harness = Harness::new();
        let mut view = record();
        view.present_fields = 1 << 8;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidConfig);

        let mut view = record();
        view.reserved_flags = 1;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let mut view = record();
        view.reserved[2] = 7;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn trace_context_is_set_explicitly_and_validated() {
        let harness = Harness::new();
        let mut view = record();
        view.present_fields = OTEL_LOG_FIELD_TRACE_CONTEXT;
        view.trace_context.trace_id = [1u8; 16];
        view.trace_context.span_id = [2u8; 8];
        view.trace_context.trace_flags = 0x03;
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        let ctx = log.record.trace_context().expect("trace context");
        assert_eq!(ctx.trace_id, TraceId::from_bytes([1u8; 16]));
        assert_eq!(ctx.span_id, SpanId::from_bytes([2u8; 8]));
        assert_eq!(ctx.trace_flags, Some(TraceFlags::new(0x03)));

        let mut bad = view;
        bad.trace_context.trace_id = [0u8; 16];
        assert_eq!(harness.emit(&bad), OtelStatus::InvalidArgument);
        let mut bad = view;
        bad.trace_context.span_id = [0u8; 8];
        assert_eq!(harness.emit(&bad), OtelStatus::InvalidArgument);
        let mut bad = view;
        bad.trace_context.trace_flags = 0x80;
        assert_eq!(harness.emit(&bad), OtelStatus::InvalidConfig);
        let mut bad = view;
        bad.trace_context.reserved[0] = 1;
        assert_eq!(harness.emit(&bad), OtelStatus::InvalidArgument);
    }

    #[test]
    fn omitting_the_trace_context_bit_leaves_the_record_uncorrelated() {
        let harness = Harness::new();
        let mut view = record();
        // Garbage in the unread field must be ignored, not consulted.
        view.trace_context.trace_id = [9u8; 16];
        view.trace_context.reserved = [7u8; 7];
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        // The pinned SDK falls back to the ambient Rust `Context`, which is empty here, so a
        // C caller never gets a silently-attached correlation.
        assert!(harness.only().record.trace_context().is_none());
    }

    #[test]
    fn scalar_bodies_and_attributes_round_trip() {
        let harness = Harness::new();
        let attrs = [
            entry("s", v_string("text")),
            entry("i", v_int(-5)),
            entry("b", v_bool(true)),
            entry("d", v_double(2.5)),
            entry("raw", v_bytes(&[0xDE, 0xAD, 0xBE, 0xEF])),
        ];
        let mut view = record();
        view.body = v_string("hello");
        view.attributes = attrs.as_ptr();
        view.attribute_count = attrs.len();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);

        let log = harness.only();
        assert_eq!(log.record.body(), Some(&AnyValue::String("hello".into())));
        let found: std::collections::HashMap<_, _> = log
            .record
            .attributes_iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.clone()))
            .collect();
        assert_eq!(found["s"], AnyValue::String("text".into()));
        assert_eq!(found["i"], AnyValue::Int(-5));
        assert_eq!(found["b"], AnyValue::Boolean(true));
        assert_eq!(found["d"], AnyValue::Double(2.5));
        assert_eq!(
            found["raw"],
            AnyValue::Bytes(Box::new(vec![0xDE, 0xAD, 0xBE, 0xEF]))
        );
    }

    #[test]
    fn nested_containers_convert_bottom_up() {
        let harness = Harness::new();
        // body = ["one", {"k": [2]}]
        let nodes = [
            element(v_string("one")),                               // 0
            element(v_container(OtelLogValueType::Map, 2, 1)),      // 1
            entry("k", v_container(OtelLogValueType::Array, 3, 1)), // 2
            element(v_int(2)),                                      // 3
        ];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 2);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);

        let log = harness.only();
        let AnyValue::ListAny(items) = log.record.body().expect("body").clone() else {
            panic!("expected a list body");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], AnyValue::String("one".into()));
        let AnyValue::Map(map) = items[1].clone() else {
            panic!("expected a map");
        };
        assert_eq!(
            map[&Key::from("k")],
            AnyValue::ListAny(Box::new(vec![AnyValue::Int(2)]))
        );
    }

    #[test]
    fn backward_and_self_child_ranges_are_rejected() {
        let harness = Harness::new();
        // Node 1 points back at node 0: a cycle would need this, so it must be refused.
        let nodes = [
            element(v_container(OtelLogValueType::Array, 1, 1)),
            element(v_container(OtelLogValueType::Array, 0, 1)),
        ];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let self_ref = [element(v_container(OtelLogValueType::Array, 0, 1))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = self_ref.as_ptr();
        view.value_node_count = self_ref.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn out_of_range_and_shared_child_ranges_are_rejected() {
        let harness = Harness::new();
        let nodes = [element(v_container(OtelLogValueType::Array, 1, 5))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        // Two parents claiming the same child.
        let shared = [
            element(v_container(OtelLogValueType::Array, 2, 1)),
            element(v_container(OtelLogValueType::Array, 2, 1)),
            element(v_int(1)),
        ];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 2);
        view.value_nodes = shared.as_ptr();
        view.value_node_count = shared.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn unreferenced_pool_nodes_are_rejected() {
        let harness = Harness::new();
        let nodes = [element(v_int(1)), element(v_int(2))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
    }

    #[test]
    fn map_and_array_key_rules_are_enforced() {
        let harness = Harness::new();
        // Array element carrying a key.
        let nodes = [entry("nope", v_int(1))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        // Map entry without a key.
        let nodes = [element(v_int(1))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Map, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        // Duplicate map keys would be silently collapsed by the pinned `HashMap`.
        let nodes = [entry("k", v_int(1)), entry("k", v_int(2))];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Map, 0, 2);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn duplicate_top_level_attribute_keys_are_preserved() {
        let harness = Harness::new();
        let attrs = [entry("k", v_int(1)), entry("k", v_int(2))];
        let mut view = record();
        view.attributes = attrs.as_ptr();
        view.attribute_count = attrs.len();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);
        let log = harness.only();
        let values: Vec<_> = log
            .record
            .attributes_iter()
            .filter(|(k, _)| k.as_str() == "k")
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(values, vec![AnyValue::Int(1), AnyValue::Int(2)]);
    }

    #[test]
    fn empty_values_are_rejected_outside_an_absent_body() {
        let harness = Harness::new();
        let attrs = [entry("k", v_empty())];
        let mut view = record();
        view.attributes = attrs.as_ptr();
        view.attribute_count = attrs.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let nodes = [element(v_empty())];
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn unknown_value_tags_and_non_zero_value_reserved_words_are_rejected() {
        let harness = Harness::new();
        let mut view = record();
        view.body = v_int(1);
        view.body.value_type = 0x1000;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let mut view = record();
        view.body = v_int(1);
        view.body.reserved = 1;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let mut view = record();
        view.body.reserved = 1;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn depth_and_count_limits_are_enforced_at_their_boundaries() {
        // A chain of `OTEL_LOG_MAX_VALUE_DEPTH` containers is the deepest accepted shape:
        // the body is depth 1, so it needs MAX-1 pool containers under it.
        let build = |containers: usize| {
            let mut nodes = Vec::new();
            for index in 0..containers {
                nodes.push(element(v_container(
                    OtelLogValueType::Array,
                    index as u32 + 1,
                    1,
                )));
            }
            nodes.push(element(v_int(0)));
            nodes
        };

        let harness = Harness::new();
        let nodes = build(OTEL_LOG_MAX_VALUE_DEPTH - 2);
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::Ok);

        let nodes = build(OTEL_LOG_MAX_VALUE_DEPTH - 1);
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, 1);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        // Attribute and node budgets.
        let attrs: Vec<_> = (0..OTEL_LOG_MAX_ATTRIBUTES + 1)
            .map(|_| entry("k", v_int(1)))
            .collect();
        let mut view = record();
        view.attributes = attrs.as_ptr();
        view.attribute_count = attrs.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let nodes: Vec<_> = (0..OTEL_LOG_MAX_VALUE_NODES + 1)
            .map(|_| element(v_int(1)))
            .collect();
        let mut view = record();
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
    }

    #[test]
    fn array_and_map_element_budgets_are_enforced() {
        let harness = Harness::new();
        let nodes: Vec<_> = (0..OTEL_LOG_MAX_ARRAY_ELEMENTS + 1)
            .map(|_| element(v_int(1)))
            .collect();
        let mut view = record();
        view.body = v_container(OtelLogValueType::Array, 0, nodes.len() as u32);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let keys: Vec<String> = (0..OTEL_LOG_MAX_MAP_ENTRIES + 1)
            .map(|index| format!("k{index}"))
            .collect();
        let nodes: Vec<_> = keys.iter().map(|k| entry(k, v_int(1))).collect();
        let mut view = record();
        view.body = v_container(OtelLogValueType::Map, 0, nodes.len() as u32);
        view.value_nodes = nodes.as_ptr();
        view.value_node_count = nodes.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn invalid_utf8_and_null_pointer_views_are_rejected() {
        let harness = Harness::new();
        let invalid = [0xFFu8, 0xFE];
        let mut view = record();
        view.body = value(
            OtelLogValueType::String,
            OtelLogValuePayload {
                string_value: OtelStringView {
                    ptr: invalid.as_ptr().cast::<c_char>(),
                    len: invalid.len(),
                },
            },
        );
        assert_eq!(harness.emit(&view), OtelStatus::InvalidUtf8);

        let mut view = record();
        view.attribute_count = 3;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let mut view = record();
        view.value_node_count = 3;
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);

        let mut view = record();
        view.body = value(
            OtelLogValueType::String,
            OtelLogValuePayload {
                string_value: OtelStringView {
                    ptr: std::ptr::null(),
                    len: 4,
                },
            },
        );
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
        assert!(harness.emitted().is_empty());
    }

    #[test]
    fn empty_attribute_keys_are_rejected() {
        let harness = Harness::new();
        let attrs = [element(v_int(1))];
        let mut view = record();
        view.attributes = attrs.as_ptr();
        view.attribute_count = attrs.len();
        assert_eq!(harness.emit(&view), OtelStatus::InvalidArgument);
    }

    #[test]
    fn null_contexts_and_undersized_records_are_rejected() {
        let harness = Harness::new();
        let view = record();
        assert_eq!(
            logger_emit(std::ptr::null_mut(), &view),
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            logger_emit(harness.logger_ctx, std::ptr::null()),
            OtelStatus::InvalidArgument
        );
        let mut small = record();
        small.struct_size -= 1;
        assert_eq!(harness.emit(&small), OtelStatus::InvalidArgument);

        assert_eq!(logger_enabled(std::ptr::null_mut(), 9), 0);
        assert!(provider_get_logger(std::ptr::null_mut(), std::ptr::null()).is_null());
        assert!(provider_retain(std::ptr::null_mut()).is_null());
        // Freeing NULL contexts must be safe.
        provider_free(std::ptr::null_mut());
        logger_free(std::ptr::null_mut());
    }

    #[test]
    fn a_larger_caller_struct_size_is_accepted() {
        let harness = Harness::new();
        #[repr(C)]
        struct Extended {
            base: OtelLogRecordView,
            future: [u64; 4],
        }
        let extended = Extended {
            base: {
                let mut base = record();
                base.struct_size = std::mem::size_of::<Extended>() as u64;
                base.severity_number = 9;
                base
            },
            future: [0xAA; 4],
        };
        assert_eq!(
            logger_emit(
                harness.logger_ctx,
                (&extended as *const Extended).cast::<OtelLogRecordView>()
            ),
            OtelStatus::Ok
        );
        assert_eq!(
            harness.only().record.severity_number(),
            Some(Severity::Info)
        );
    }

    #[test]
    fn provider_retain_yields_an_independent_reference() {
        let exporter = InMemoryLogExporter::default();
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(SimpleLogProcessor::new(exporter.clone()))
            .build();
        let first = provider_ctx(provider);
        let second = provider_retain(first);
        assert!(!second.is_null());
        assert_ne!(first, second);

        provider_free(first);
        // The second reference must still produce a working logger.
        let scope = OtelScopeConfig {
            name: sv("after-release"),
            version: OtelStringView::empty(),
            schema_url: OtelStringView::empty(),
            attributes: std::ptr::null(),
            attribute_count: 0,
        };
        let logger = provider_get_logger(second, &scope);
        assert!(!logger.is_null());
        let mut view = record();
        view.body = v_string("still alive");
        assert_eq!(logger_emit(logger, &view), OtelStatus::Ok);
        logger_free(logger);
        // Read before releasing the last reference: dropping it shuts the provider down, and
        // the in-memory exporter clears its buffer on shutdown.
        let logs = exporter.get_emitted_logs().expect("emitted logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].instrumentation.name(), "after-release");
        provider_free(second);
    }

    #[test]
    fn enabled_reports_false_for_severities_the_pinned_check_cannot_express() {
        let harness = Harness::new();
        assert_eq!(logger_enabled(harness.logger_ctx, 0), 0);
        assert_eq!(logger_enabled(harness.logger_ctx, 25), 0);
        // With a processor that does not override `event_enabled`, everything in range is on.
        for severity in 1..=24u32 {
            assert_eq!(logger_enabled(harness.logger_ctx, severity), 1);
        }
    }

    #[test]
    fn the_logs_vtable_advertises_its_own_abi_kind_and_size() {
        assert_eq!(SDK_LOGS_VTABLE.abi_version, OTEL_LOGS_IMPL_ABI_VERSION);
        assert_eq!(
            SDK_LOGS_VTABLE.struct_size,
            std::mem::size_of::<OtelLogsVtable>()
        );
        assert!(unsafe { opentelemetry_c_abi::logs_vtable_compatible(vtable_ptr()) });
    }
}
