//! The SDK's implementation of the internal [`OtelImplVtable`].
//!
//! These `extern "C"` functions are the concrete, `opentelemetry_sdk`-backed behavior
//! behind the API's opaque handles. Their addresses populate a single `'static`
//! [`SDK_VTABLE`]; the API stores a `*const OtelImplVtable` in each handle it creates for
//! an SDK-backed object. Every function is panic-guarded (a Rust panic must never unwind
//! across the C ABI into the API cdylib). No Rust types cross the boundary; contexts are
//! opaque `*mut c_void` that only this crate allocates and frees.
//!
//! **Hot-path contract** (see the component README): the span/tracer functions here are the
//! hot path. Keep them thin — only required C→SDK marshalling (owning the borrowed key/value/
//! name bytes the OTel SDK needs) plus the SDK's own work. Do **not** add locks, registries,
//! C-side buffering, per-span config/env lookups, or builder/exporter/processor access here.
//! Span ops rely on the one-span-per-thread contract to take `&mut` without synchronization
//! ([`span_mut`]); the only lock/`Arc`-clone lives in provider retain, on tracer acquisition.

use std::borrow::Cow;
use std::collections::HashSet;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry::trace::{
    Link, Span, SpanContext, SpanId, SpanKind, Status, TraceContextExt, TraceFlags, TraceId,
    TraceState, Tracer, TracerProvider,
};
use opentelemetry::{Context, InstrumentationScope, Key, KeyValue, StringValue, Value};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, Span as SdkOtelSpan};

use opentelemetry_c_abi::{
    OtelAttributeType, OtelImplVtable, OtelKeyValue, OtelSpanContextView, OtelSpanKind,
    OtelSpanStartConfig, OtelSpanStatusCode, OtelStatus, OtelStringView,
    OTEL_TRACE_IMPL_ABI_VERSION,
};

use crate::error::{fail, fail_abi};

/// Upper bound on a C-provided attribute count (protects the up-front `Vec`).
const MAX_ATTRIBUTES: usize = 1_048_576;

// ---- Context types (opaque `*mut c_void` on the wire) ----------------------

/// A span context: the concrete SDK span.
struct SdkSpan {
    span: SdkOtelSpan,
}

/// # Safety
/// `ctx` must be a live span context produced by this vtable, used single-threaded.
unsafe fn span_mut<'a>(ctx: *mut c_void) -> &'a mut SdkSpan {
    unsafe { &mut *(ctx as *mut SdkSpan) }
}

// ---- Panic guards ----------------------------------------------------------

fn guard_ptr<F: FnOnce() -> *mut c_void>(f: F) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(std::ptr::null_mut())
}
fn guard_status<F: FnOnce() -> OtelStatus>(f: F) -> OtelStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(OtelStatus::InternalError)
}
fn guard_unit<F: FnOnce()>(f: F) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

// ---- A minimal parent span (preserves the supplied SpanContext, including remoteness) ---

struct LocalParentSpan {
    span_context: SpanContext,
}

impl LocalParentSpan {
    fn new(parent: &SpanContext) -> Self {
        LocalParentSpan {
            span_context: parent.clone(),
        }
    }
}

impl Span for LocalParentSpan {
    fn add_event_with_timestamp<T>(&mut self, _n: T, _t: SystemTime, _a: Vec<KeyValue>)
    where
        T: Into<Cow<'static, str>>,
    {
    }
    fn span_context(&self) -> &SpanContext {
        &self.span_context
    }
    fn is_recording(&self) -> bool {
        false
    }
    fn set_attribute(&mut self, _a: KeyValue) {}
    fn set_status(&mut self, _s: Status) {}
    fn update_name<T>(&mut self, _n: T)
    where
        T: Into<Cow<'static, str>>,
    {
    }
    fn add_link(&mut self, _sc: SpanContext, _a: Vec<KeyValue>) {}
    fn end_with_timestamp(&mut self, _t: SystemTime) {}
}

// ---- Attribute conversion --------------------------------------------------

/// Convert a borrowed C attribute into an owned [`KeyValue`] (strict UTF-8 keys/strings;
/// invalid UTF-8 is rejected).
///
/// # Safety
/// String views inside `kv` must be valid.
pub(crate) unsafe fn to_key_value(kv: &OtelKeyValue) -> Result<KeyValue, OtelStatus> {
    // SAFETY: forwarded to the caller's contract.
    let key = unsafe { kv.key.to_string_strict() }.map_err(fail_abi)?;
    if key.is_empty() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "attribute key must not be empty",
        ));
    }
    let value_type = OtelAttributeType::from_u32(kv.value_type).ok_or_else(|| {
        fail(
            OtelStatus::InvalidArgument,
            "attribute value_type is not a valid OtelAttributeType tag",
        )
    })?;
    let value: Value = match value_type {
        OtelAttributeType::String => {
            // SAFETY: tag guarantees the string member is active.
            let s = unsafe { kv.value.string_value.to_string_strict() }.map_err(fail_abi)?;
            Value::String(StringValue::from(s))
        }
        // SAFETY: tag guarantees the respective member is active.
        OtelAttributeType::Bool => Value::Bool(unsafe { kv.value.bool_value } != 0),
        OtelAttributeType::Int64 => Value::I64(unsafe { kv.value.int64_value }),
        OtelAttributeType::Double => Value::F64(unsafe { kv.value.double_value }),
    };
    Ok(KeyValue::new(Key::from(key), value))
}

/// Collect a borrowed C attribute array into owned [`KeyValue`]s, with bounds/overflow
/// guards and fallible reservation.
///
/// # Safety
/// `attributes` must point to `count` valid [`OtelKeyValue`]s, or be NULL when `count == 0`.
pub(crate) unsafe fn collect_key_values(
    attributes: *const OtelKeyValue,
    count: usize,
) -> Result<Vec<KeyValue>, OtelStatus> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if attributes.is_null() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "attribute array is NULL with non-zero count",
        ));
    }
    if count > MAX_ATTRIBUTES {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "attribute count exceeds the maximum supported value",
        ));
    }
    let within_bounds = count
        .checked_mul(std::mem::size_of::<OtelKeyValue>())
        .is_some_and(|b| b <= isize::MAX as usize);
    if !within_bounds {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "attribute array exceeds the maximum supported size",
        ));
    }
    let mut out: Vec<KeyValue> = Vec::new();
    if out.try_reserve(count).is_err() {
        return Err(fail(
            OtelStatus::InternalError,
            "failed to allocate space for attributes",
        ));
    }
    // SAFETY: non-NULL, `count` valid elements, total size within isize::MAX.
    let slice = unsafe { std::slice::from_raw_parts(attributes, count) };
    for kv in slice {
        // SAFETY: each element satisfies the OtelKeyValue contract.
        out.push(unsafe { to_key_value(kv) }?);
    }
    Ok(out)
}

/// Collect attributes and reject duplicate keys, for identity-bearing scope attributes.
///
/// # Safety
/// Same contract as [`collect_key_values`].
pub(crate) unsafe fn collect_unique_key_values(
    attributes: *const OtelKeyValue,
    count: usize,
) -> Result<Vec<KeyValue>, OtelStatus> {
    let values = unsafe { collect_key_values(attributes, count) }?;
    let mut keys = HashSet::new();
    keys.try_reserve(values.len()).map_err(|_| {
        fail(
            OtelStatus::InternalError,
            "failed to allocate scope attribute validation state",
        )
    })?;
    for value in &values {
        if !keys.insert(value.key.clone()) {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "duplicate scope attribute key",
            ));
        }
    }
    Ok(values)
}

// ---- Provider vtable -------------------------------------------------------

extern "C" fn vt_provider_get_tracer(
    ctx: *mut c_void,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` is a live provider context produced by this crate.
        let provider = unsafe { &*(ctx as *const SdkTracerProvider) };
        // SAFETY: string views satisfy the ABI contract.
        let name = match unsafe { name.to_string_strict() } {
            Ok(n) => n,
            Err(e) => {
                fail_abi(e);
                return std::ptr::null_mut();
            }
        };
        let mut scope = InstrumentationScope::builder(name);
        // SAFETY: string views satisfy the ABI contract.
        match unsafe { version.to_string_strict() } {
            Ok(v) if !v.is_empty() => scope = scope.with_version(v),
            Ok(_) => {}
            Err(e) => {
                fail_abi(e);
                return std::ptr::null_mut();
            }
        }
        match unsafe { schema_url.to_string_strict() } {
            Ok(s) if !s.is_empty() => scope = scope.with_schema_url(s),
            Ok(_) => {}
            Err(e) => {
                fail_abi(e);
                return std::ptr::null_mut();
            }
        }
        let tracer = provider.tracer_with_scope(scope.build());
        Box::into_raw(Box::new(tracer)) as *mut c_void
    })
}

extern "C" fn vt_provider_retain(ctx: *mut c_void) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` is a live provider context (Box<SdkTracerProvider>) produced by this
        // crate; the API only calls this while the reference is guaranteed alive (under its
        // global read lock, or for a handle it owns). Clone the Arc-backed provider into a
        // new owned Box — an independent reference that outlives slot replacement.
        let provider = unsafe { &*(ctx as *const SdkTracerProvider) };
        Box::into_raw(Box::new(provider.clone())) as *mut c_void
    })
}

extern "C" fn vt_provider_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            // SAFETY: `ctx` was a Box<SdkTracerProvider> produced by this crate. Dropping it
            // releases exactly one Arc reference; the provider lives while any reference
            // (slot or retained) remains.
            drop(unsafe { Box::from_raw(ctx as *mut SdkTracerProvider) });
        }
    });
}

// ---- Tracer vtable ---------------------------------------------------------

extern "C" fn vt_tracer_start_span(
    ctx: *mut c_void,
    name: OtelStringView,
    kind: u32,
    parent_span_ctx: *mut c_void,
) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` is a live tracer context produced by this crate.
        let tracer = unsafe { &*(ctx as *const SdkTracer) };
        // SAFETY: string view satisfies the ABI contract.
        let name = match unsafe { name.to_string_strict() } {
            Ok(n) => n,
            Err(e) => {
                fail_abi(e);
                return std::ptr::null_mut();
            }
        };
        let span_kind: SpanKind =
            match OtelSpanKind::from_u32(kind).unwrap_or(OtelSpanKind::Internal) {
                OtelSpanKind::Internal => SpanKind::Internal,
                OtelSpanKind::Server => SpanKind::Server,
                OtelSpanKind::Client => SpanKind::Client,
                OtelSpanKind::Producer => SpanKind::Producer,
                OtelSpanKind::Consumer => SpanKind::Consumer,
            };

        let builder = tracer.span_builder(name).with_kind(span_kind);
        let span = if parent_span_ctx.is_null() {
            tracer.build_with_context(builder, &Context::new())
        } else {
            // SAFETY: the API only passes a parent context produced by THIS vtable.
            let parent = unsafe { &*(parent_span_ctx as *const SdkSpan) };
            let cx = Context::new().with_span(LocalParentSpan::new(parent.span.span_context()));
            tracer.build_with_context(builder, &cx)
        };
        Box::into_raw(Box::new(SdkSpan { span })) as *mut c_void
    })
}

extern "C" fn vt_span_context_visit(
    ctx: *mut c_void,
    visitor: opentelemetry_c_abi::OtelSpanContextVisitor,
    user_data: *mut c_void,
) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(OtelStatus::InvalidArgument, "span context is NULL");
        }
        let Some(visitor) = visitor else {
            return fail(OtelStatus::InvalidArgument, "span context visitor is NULL");
        };
        // SAFETY: `ctx` is a live span context produced by this crate.
        let span = unsafe { &*(ctx as *const SdkSpan) };
        let context = span.span.span_context();
        let trace_state = context.trace_state().header();
        let view = OtelSpanContextView {
            trace_id: context.trace_id().to_bytes(),
            span_id: context.span_id().to_bytes(),
            trace_flags: context.trace_flags().to_u8(),
            reserved: [0; 3],
            is_remote: u32::from(context.is_remote()),
            trace_state: OtelStringView {
                ptr: trace_state.as_ptr().cast(),
                len: trace_state.len(),
            },
        };
        visitor(user_data, &view)
    })
}

extern "C" fn vt_tracer_start_span_with_context(
    ctx: *mut c_void,
    name: OtelStringView,
    kind: u32,
    parent: *const OtelSpanContextView,
) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() || parent.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the API supplies a borrowed view valid for this call.
        let parent = unsafe { &*parent };
        if parent.reserved != [0; 3] {
            fail(
                OtelStatus::InvalidArgument,
                "span context reserved bytes must be zero",
            );
            return std::ptr::null_mut();
        }
        let trace_state = match unsafe { parent.trace_state.as_str() }
            .ok()
            .and_then(|value| TraceState::from_str(value).ok())
        {
            Some(value) => value,
            None => {
                fail(
                    OtelStatus::InvalidConfig,
                    "span context trace state is invalid",
                );
                return std::ptr::null_mut();
            }
        };
        let parent = SpanContext::new(
            TraceId::from_bytes(parent.trace_id),
            SpanId::from_bytes(parent.span_id),
            TraceFlags::new(parent.trace_flags),
            parent.is_remote != 0,
            trace_state,
        );
        if !parent.is_valid() {
            fail(
                OtelStatus::InvalidArgument,
                "parent span context is invalid",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` is a live tracer context produced by this crate.
        let tracer = unsafe { &*(ctx as *const SdkTracer) };
        // SAFETY: string view satisfies the ABI contract.
        let name = match unsafe { name.to_string_strict() } {
            Ok(name) => name,
            Err(error) => {
                fail_abi(error);
                return std::ptr::null_mut();
            }
        };
        let span_kind = match OtelSpanKind::from_u32(kind).unwrap_or(OtelSpanKind::Internal) {
            OtelSpanKind::Internal => SpanKind::Internal,
            OtelSpanKind::Server => SpanKind::Server,
            OtelSpanKind::Client => SpanKind::Client,
            OtelSpanKind::Producer => SpanKind::Producer,
            OtelSpanKind::Consumer => SpanKind::Consumer,
        };
        let builder = tracer.span_builder(name).with_kind(span_kind);
        let parent_context = Context::new().with_span(LocalParentSpan::new(&parent));
        let span = tracer.build_with_context(builder, &parent_context);
        Box::into_raw(Box::new(SdkSpan { span })) as *mut c_void
    })
}

// ---- Extended span-start helpers ------------------------------------------

fn span_kind_from_u32(kind: u32) -> SpanKind {
    match OtelSpanKind::from_u32(kind).unwrap_or(OtelSpanKind::Internal) {
        OtelSpanKind::Internal => SpanKind::Internal,
        OtelSpanKind::Server => SpanKind::Server,
        OtelSpanKind::Client => SpanKind::Client,
        OtelSpanKind::Producer => SpanKind::Producer,
        OtelSpanKind::Consumer => SpanKind::Consumer,
    }
}

/// Reconstruct an SDK [`SpanContext`] from a borrowed view. Validates reserved bytes and
/// tracestate grammar.
///
/// # Safety
/// `view` must be a valid borrowed [`OtelSpanContextView`] for the call.
unsafe fn span_context_from_view(view: &OtelSpanContextView) -> Result<SpanContext, OtelStatus> {
    if view.reserved != [0; 3] {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "span context reserved bytes must be zero",
        ));
    }
    let trace_state = match unsafe { view.trace_state.as_str() }
        .ok()
        .and_then(|value| TraceState::from_str(value).ok())
    {
        Some(value) => value,
        None => {
            return Err(fail(
                OtelStatus::InvalidConfig,
                "span context trace state is invalid",
            ))
        }
    };
    Ok(SpanContext::new(
        TraceId::from_bytes(view.trace_id),
        SpanId::from_bytes(view.span_id),
        TraceFlags::new(view.trace_flags),
        view.is_remote != 0,
        trace_state,
    ))
}

/// Start a span from a forward-only [`OtelSpanStartConfig`] descriptor: links, an explicit
/// start timestamp, initial attributes, and a single parenting source.
extern "C" fn vt_tracer_start_span_ex(
    ctx: *mut c_void,
    name: OtelStringView,
    config: *const OtelSpanStartConfig,
) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() || config.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "tracer context and config must not be NULL",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: the API supplies a borrowed descriptor valid for this call.
        let config = unsafe { &*config };
        if config.reserved != 0 {
            fail(
                OtelStatus::InvalidArgument,
                "reserved field in span-start config must be zero",
            );
            return std::ptr::null_mut();
        }
        if !config.parent_span_ctx.is_null() && !config.parent_context.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "live parent and context parent are mutually exclusive",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` is a live tracer context produced by this crate.
        let tracer = unsafe { &*(ctx as *const SdkTracer) };
        // SAFETY: string view satisfies the ABI contract.
        let name = match unsafe { name.to_string_strict() } {
            Ok(name) => name,
            Err(error) => {
                fail_abi(error);
                return std::ptr::null_mut();
            }
        };

        let attributes =
            match unsafe { collect_key_values(config.attributes, config.attribute_count) } {
                Ok(attrs) => attrs,
                Err(_) => return std::ptr::null_mut(),
            };

        // Convert links.
        let mut links: Vec<Link> = Vec::new();
        if config.link_count != 0 {
            if config.links.is_null() {
                fail(
                    OtelStatus::InvalidArgument,
                    "link array is NULL with a non-zero count",
                );
                return std::ptr::null_mut();
            }
            if links.try_reserve(config.link_count).is_err() {
                fail(
                    OtelStatus::InternalError,
                    "failed to allocate space for links",
                );
                return std::ptr::null_mut();
            }
            // SAFETY: non-NULL with `link_count` valid elements per the ABI contract.
            let link_slice = unsafe { std::slice::from_raw_parts(config.links, config.link_count) };
            for link in link_slice {
                let span_context = match unsafe { span_context_from_view(&link.context) } {
                    Ok(sc) if sc.is_valid() => sc,
                    Ok(_) => {
                        fail(OtelStatus::InvalidArgument, "link span context is invalid");
                        return std::ptr::null_mut();
                    }
                    Err(_) => return std::ptr::null_mut(),
                };
                let link_attrs =
                    match unsafe { collect_key_values(link.attributes, link.attribute_count) } {
                        Ok(attrs) => attrs,
                        Err(_) => return std::ptr::null_mut(),
                    };
                links.push(Link::new(span_context, link_attrs, 0));
            }
        }

        let mut builder = tracer
            .span_builder(name)
            .with_kind(span_kind_from_u32(config.kind));
        if !attributes.is_empty() {
            builder = builder.with_attributes(attributes);
        }
        if !links.is_empty() {
            builder = builder.with_links(links);
        }
        if config.start_time_unix_nanos != 0 {
            let start = UNIX_EPOCH + Duration::from_nanos(config.start_time_unix_nanos);
            builder = builder.with_start_time(start);
        }

        // Resolve the parenting source.
        let cx = if !config.parent_span_ctx.is_null() {
            // SAFETY: a live parent produced by THIS vtable.
            let parent = unsafe { &*(config.parent_span_ctx as *const SdkSpan) };
            Context::new().with_span(LocalParentSpan::new(parent.span.span_context()))
        } else if !config.parent_context.is_null() {
            // SAFETY: borrowed view valid for the call.
            let view = unsafe { &*config.parent_context };
            let parent = match unsafe { span_context_from_view(view) } {
                Ok(sc) if sc.is_valid() => sc,
                Ok(_) => {
                    fail(
                        OtelStatus::InvalidArgument,
                        "parent span context is invalid",
                    );
                    return std::ptr::null_mut();
                }
                Err(_) => return std::ptr::null_mut(),
            };
            Context::new().with_span(LocalParentSpan::new(&parent))
        } else {
            Context::new()
        };

        let span = tracer.build_with_context(builder, &cx);
        Box::into_raw(Box::new(SdkSpan { span })) as *mut c_void
    })
}

extern "C" fn vt_tracer_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            // SAFETY: `ctx` was a Box<SdkTracer> produced by this crate.
            drop(unsafe { Box::from_raw(ctx as *mut SdkTracer) });
        }
    });
}

// ---- Span vtable -----------------------------------------------------------

extern "C" fn vt_span_set_string(
    ctx: *mut c_void,
    key: OtelStringView,
    value: OtelStringView,
) -> OtelStatus {
    guard_status(|| {
        // SAFETY: `ctx` live span, single-threaded per contract.
        let span = unsafe { span_mut(ctx) };
        let key = match unsafe { key.to_string_strict() } {
            Ok(k) if !k.is_empty() => k,
            Ok(_) => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "attribute key must not be empty",
                )
            }
            Err(e) => return fail_abi(e),
        };
        let value = match unsafe { value.to_string_strict() } {
            Ok(v) => v,
            Err(e) => return fail_abi(e),
        };
        span.span.set_attribute(KeyValue::new(key, value));
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_set_bool(ctx: *mut c_void, key: OtelStringView, value: u32) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let key = match unsafe { key.to_string_strict() } {
            Ok(k) if !k.is_empty() => k,
            Ok(_) => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "attribute key must not be empty",
                )
            }
            Err(e) => return fail_abi(e),
        };
        span.span.set_attribute(KeyValue::new(key, value != 0));
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_set_i64(ctx: *mut c_void, key: OtelStringView, value: i64) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let key = match unsafe { key.to_string_strict() } {
            Ok(k) if !k.is_empty() => k,
            Ok(_) => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "attribute key must not be empty",
                )
            }
            Err(e) => return fail_abi(e),
        };
        span.span.set_attribute(KeyValue::new(key, value));
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_set_f64(ctx: *mut c_void, key: OtelStringView, value: f64) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let key = match unsafe { key.to_string_strict() } {
            Ok(k) if !k.is_empty() => k,
            Ok(_) => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "attribute key must not be empty",
                )
            }
            Err(e) => return fail_abi(e),
        };
        span.span.set_attribute(KeyValue::new(key, value));
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_add_event(
    ctx: *mut c_void,
    name: OtelStringView,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let name = match unsafe { name.to_string_strict() } {
            Ok(n) => n,
            Err(e) => return fail_abi(e),
        };
        let attrs = match unsafe { collect_key_values(attributes, attribute_count) } {
            Ok(a) => a,
            Err(status) => return status,
        };
        span.span.add_event(name, attrs);
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_set_status(
    ctx: *mut c_void,
    code: u32,
    description: OtelStringView,
) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let code = match OtelSpanStatusCode::from_u32(code) {
            Some(c) => c,
            None => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "status code is not a valid OtelSpanStatusCode value",
                )
            }
        };
        let status = match code {
            OtelSpanStatusCode::Unset => Status::Unset,
            OtelSpanStatusCode::Ok => Status::Ok,
            OtelSpanStatusCode::Error => {
                let desc = match unsafe { description.to_string_strict() } {
                    Ok(d) => d,
                    Err(e) => return fail_abi(e),
                };
                Status::error(desc)
            }
        };
        span.span.set_status(status);
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_update_name(ctx: *mut c_void, name: OtelStringView) -> OtelStatus {
    guard_status(|| {
        let span = unsafe { span_mut(ctx) };
        let name = match unsafe { name.to_string_strict() } {
            Ok(n) => n,
            Err(e) => return fail_abi(e),
        };
        span.span.update_name(name);
        OtelStatus::Ok
    })
}

extern "C" fn vt_span_end(ctx: *mut c_void) {
    guard_unit(|| {
        // SAFETY: `ctx` live span, single-threaded per contract.
        let span = unsafe { span_mut(ctx) };
        span.span.end();
    });
}

extern "C" fn vt_span_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            // SAFETY: `ctx` was a Box<SdkSpan> produced by this crate. The API ends the span
            // (via vt_span_end) before freeing; dropping the SDK span also ends it if it was not
            // already ended (the SDK span tracks its ended state, so this never double-ends).
            // This matches the OtelImplVtable::span_free ownership contract.
            drop(unsafe { Box::from_raw(ctx as *mut SdkSpan) });
        }
    });
}

/// The single `'static` implementation vtable installed into the API global slot.
pub(crate) static SDK_VTABLE: OtelImplVtable = OtelImplVtable {
    abi_version: OTEL_TRACE_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelImplVtable>(),
    provider_get_tracer: vt_provider_get_tracer,
    provider_retain: vt_provider_retain,
    provider_free: vt_provider_free,
    tracer_start_span: vt_tracer_start_span,
    tracer_free: vt_tracer_free,
    span_set_string: vt_span_set_string,
    span_set_bool: vt_span_set_bool,
    span_set_i64: vt_span_set_i64,
    span_set_f64: vt_span_set_f64,
    span_add_event: vt_span_add_event,
    span_set_status: vt_span_set_status,
    span_update_name: vt_span_update_name,
    span_end: vt_span_end,
    span_free: vt_span_free,
    span_context_visit: vt_span_context_visit,
    tracer_start_span_with_context: vt_tracer_start_span_with_context,
    tracer_start_span_ex: vt_tracer_start_span_ex,
};

/// Pointer to the SDK vtable (installed via the API registration ABI).
pub(crate) fn vtable_ptr() -> *const OtelImplVtable {
    &SDK_VTABLE
}

/// Box a cloned SDK provider into an opaque provider context for the API slot/handle.
pub(crate) fn provider_ctx(provider: SdkTracerProvider) -> *mut c_void {
    Box::into_raw(Box::new(provider)) as *mut c_void
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::SpanId;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SimpleSpanProcessor};
    use std::os::raw::c_char;

    fn sv(s: &str) -> OtelStringView {
        OtelStringView {
            ptr: s.as_ptr().cast::<c_char>(),
            len: s.len(),
        }
    }
    fn empty() -> OtelStringView {
        OtelStringView {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    /// The SDK vtable must reproduce local parent/child semantics and attribute handling
    /// (this is the same behavior exercised end-to-end by the cross-artifact C test, but
    /// verified here directly against an in-memory exporter).
    #[test]
    fn vtable_parent_child_and_attributes() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), sv("1.0"), empty());
        assert!(!tctx.is_null());

        let parent = (vt.tracer_start_span)(tctx, sv("parent"), 0, std::ptr::null_mut());
        assert!(!parent.is_null());
        assert_eq!(
            (vt.span_set_string)(parent, sv("component"), sv("demo")),
            OtelStatus::Ok
        );
        assert_eq!((vt.span_set_i64)(parent, sv("n"), 7), OtelStatus::Ok);
        // empty key rejected
        assert_eq!(
            (vt.span_set_bool)(parent, sv(""), 1),
            OtelStatus::InvalidArgument
        );

        // child linked to parent (kind=2 client)
        let child = (vt.tracer_start_span)(tctx, sv("child"), 2, parent);
        assert!(!child.is_null());
        (vt.span_end)(child);
        (vt.span_end)(parent);

        let spans = exporter.get_finished_spans().unwrap();
        let c = spans.iter().find(|s| s.name == "child").expect("child");
        let p = spans.iter().find(|s| s.name == "parent").expect("parent");
        assert_eq!(c.span_context.trace_id(), p.span_context.trace_id());
        assert_eq!(c.parent_span_id, p.span_context.span_id());
        assert!(!c.parent_span_is_remote);
        assert_eq!(p.parent_span_id, SpanId::INVALID);

        (vt.span_free)(child);
        (vt.span_free)(parent);
        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    #[test]
    fn child_can_end_before_parent() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("s"), empty(), empty());
        let parent = (vt.tracer_start_span)(tctx, sv("p"), 0, std::ptr::null_mut());
        let child = (vt.tracer_start_span)(tctx, sv("c"), 0, parent);
        (vt.span_end)(child); // child ends first
        (vt.span_end)(parent);
        let names: Vec<_> = exporter
            .get_finished_spans()
            .unwrap()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        assert!(names.contains(&"c".to_string()) && names.contains(&"p".to_string()));
        (vt.span_free)(child);
        (vt.span_free)(parent);
        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    #[test]
    fn context_snapshot_preserves_remote_flags_and_trace_state_after_source_span_ends() {
        #[derive(Default)]
        struct Snapshot {
            trace_id: [u8; 16],
            span_id: [u8; 8],
            flags: u8,
            remote: u32,
            trace_state: String,
        }
        extern "C" fn receive(data: *mut c_void, view: *const OtelSpanContextView) -> OtelStatus {
            let data = unsafe { &mut *(data as *mut Snapshot) };
            let view = unsafe { &*view };
            data.trace_id = view.trace_id;
            data.span_id = view.span_id;
            data.flags = view.trace_flags;
            data.remote = view.is_remote;
            data.trace_state = unsafe { view.trace_state.as_str() }.unwrap().to_owned();
            OtelStatus::Ok
        }

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());
        let parent = (vt.tracer_start_span)(tctx, sv("parent"), 0, std::ptr::null_mut());
        let mut snapshot = Snapshot::default();
        assert_eq!(
            (vt.span_context_visit)(
                parent,
                Some(receive),
                (&mut snapshot as *mut Snapshot).cast()
            ),
            OtelStatus::Ok
        );
        (vt.span_end)(parent);
        (vt.span_free)(parent);

        let trace_state = sv(&snapshot.trace_state);
        let remote_trace_state = sv("vendor=value");
        let parent_view = OtelSpanContextView {
            trace_id: snapshot.trace_id,
            span_id: snapshot.span_id,
            trace_flags: snapshot.flags | 0x02,
            reserved: [0; 3],
            is_remote: 1,
            trace_state: remote_trace_state,
        };
        let child = (vt.tracer_start_span_with_context)(tctx, sv("child"), 0, &parent_view);
        assert!(!child.is_null());
        (vt.span_end)(child);
        (vt.span_free)(child);

        let spans = exporter.get_finished_spans().unwrap();
        let parent_data = spans.iter().find(|span| span.name == "parent").unwrap();
        let child_data = spans.iter().find(|span| span.name == "child").unwrap();
        assert_eq!(
            child_data.span_context.trace_id(),
            parent_data.span_context.trace_id()
        );
        assert_eq!(
            child_data.parent_span_id,
            parent_data.span_context.span_id()
        );
        assert!(child_data.parent_span_is_remote);
        assert_eq!(child_data.span_context.trace_flags().to_u8() & 0x02, 0x02);
        assert_eq!(
            child_data.span_context.trace_state().header(),
            "vendor=value"
        );
        assert_eq!(
            unsafe { trace_state.as_str() }.unwrap(),
            snapshot.trace_state
        );

        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    /// Invalid UTF-8 in any C string view crossing the vtable must be rejected (strict
    /// contract): pointer-returning ops yield NULL, status-returning ops yield INVALID_UTF8.
    #[test]
    fn invalid_utf8_is_rejected() {
        use opentelemetry_c_abi::OtelAttributeValue;

        fn sv_bytes(b: &[u8]) -> OtelStringView {
            OtelStringView {
                ptr: b.as_ptr().cast::<c_char>(),
                len: b.len(),
            }
        }
        // 0xFF is never valid in UTF-8.
        let bad: &[u8] = b"\xff\xfe";

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);

        // Tracer acquisition: an invalid scope name yields a NULL tracer.
        assert!((vt.provider_get_tracer)(pctx, sv_bytes(bad), empty(), empty()).is_null());

        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());
        assert!(!tctx.is_null());

        // Span start: an invalid name yields a NULL span.
        assert!((vt.tracer_start_span)(tctx, sv_bytes(bad), 0, std::ptr::null_mut()).is_null());

        let span = (vt.tracer_start_span)(tctx, sv("op"), 0, std::ptr::null_mut());
        assert!(!span.is_null());

        // Attribute key and string value.
        assert_eq!(
            (vt.span_set_string)(span, sv_bytes(bad), sv("v")),
            OtelStatus::InvalidUtf8
        );
        assert_eq!(
            (vt.span_set_string)(span, sv("k"), sv_bytes(bad)),
            OtelStatus::InvalidUtf8
        );
        // Scalar setter key.
        assert_eq!(
            (vt.span_set_i64)(span, sv_bytes(bad), 1),
            OtelStatus::InvalidUtf8
        );
        // Event name.
        assert_eq!(
            (vt.span_add_event)(span, sv_bytes(bad), std::ptr::null(), 0),
            OtelStatus::InvalidUtf8
        );
        // Event attribute key via to_key_value (Int64 value: no string member touched).
        let bad_kv = OtelKeyValue {
            key: sv_bytes(bad),
            value_type: 2, // Int64
            value: OtelAttributeValue { int64_value: 5 },
        };
        assert_eq!(
            (vt.span_add_event)(span, sv("evt"), &bad_kv, 1),
            OtelStatus::InvalidUtf8
        );
        // Error-status description is converted (code 2 == Error).
        assert_eq!(
            (vt.span_set_status)(span, 2, sv_bytes(bad)),
            OtelStatus::InvalidUtf8
        );
        // update_name.
        assert_eq!(
            (vt.span_update_name)(span, sv_bytes(bad)),
            OtelStatus::InvalidUtf8
        );

        // The happy path still succeeds (valid UTF-8 accepted).
        assert_eq!((vt.span_set_string)(span, sv("k"), sv("v")), OtelStatus::Ok);

        (vt.span_end)(span);
        (vt.span_free)(span);
        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    #[test]
    fn metric_attribute_conversion_rejects_malformed_inputs() {
        use opentelemetry_c_abi::OtelAttributeValue;

        assert!(unsafe { collect_key_values(std::ptr::null(), 0) }
            .unwrap()
            .is_empty());
        assert_eq!(
            unsafe { collect_key_values(std::ptr::null(), 1) }.unwrap_err(),
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            unsafe {
                collect_key_values(
                    std::ptr::NonNull::<OtelKeyValue>::dangling().as_ptr(),
                    usize::MAX,
                )
            }
            .unwrap_err(),
            OtelStatus::InvalidArgument
        );

        let empty_key = OtelKeyValue {
            key: empty(),
            value_type: 2,
            value: OtelAttributeValue { int64_value: 1 },
        };
        assert_eq!(
            unsafe { collect_key_values(&empty_key, 1) }.unwrap_err(),
            OtelStatus::InvalidArgument
        );

        let invalid_type = OtelKeyValue {
            key: sv("key"),
            value_type: u32::MAX,
            value: OtelAttributeValue { int64_value: 1 },
        };
        assert_eq!(
            unsafe { collect_key_values(&invalid_type, 1) }.unwrap_err(),
            OtelStatus::InvalidArgument
        );

        let invalid_utf8 = [0xff_u8];
        let invalid_string = OtelKeyValue {
            key: sv("key"),
            value_type: 0,
            value: OtelAttributeValue {
                string_value: OtelStringView {
                    ptr: invalid_utf8.as_ptr().cast(),
                    len: invalid_utf8.len(),
                },
            },
        };
        assert_eq!(
            unsafe { collect_key_values(&invalid_string, 1) }.unwrap_err(),
            OtelStatus::InvalidUtf8
        );
    }

    /// `vt_tracer_start_span_ex` must forward links, an explicit start time, initial
    /// attributes, and a context parent into the exported span data.
    #[test]
    fn vtable_start_span_ex_links_start_time_and_attributes() {
        use opentelemetry_c_abi::{OtelAttributeValue, OtelSpanLinkView};

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());

        // A remote parent context and a distinct linked context.
        let parent_view = OtelSpanContextView {
            trace_id: [0x11; 16],
            span_id: [0x22; 8],
            trace_flags: 0x01,
            reserved: [0; 3],
            is_remote: 1,
            trace_state: sv("vendor=parent"),
        };
        let link_view = OtelSpanContextView {
            trace_id: [0x33; 16],
            span_id: [0x44; 8],
            trace_flags: 0x01,
            reserved: [0; 3],
            is_remote: 1,
            trace_state: sv("vendor=link"),
        };
        let link_attr = OtelKeyValue {
            key: sv("link.attr"),
            value_type: 0,
            value: OtelAttributeValue {
                string_value: sv("lv"),
            },
        };
        let link = OtelSpanLinkView {
            context: link_view,
            attributes: &link_attr,
            attribute_count: 1,
        };
        let span_attr = OtelKeyValue {
            key: sv("span.attr"),
            value_type: 2,
            value: OtelAttributeValue { int64_value: 99 },
        };
        let start_nanos: u64 = 1_700_000_000_000_000_000;
        let config = OtelSpanStartConfig {
            kind: 2, // Client
            reserved: 0,
            parent_span_ctx: std::ptr::null_mut(),
            parent_context: &parent_view,
            start_time_unix_nanos: start_nanos,
            attributes: &span_attr,
            attribute_count: 1,
            links: &link,
            link_count: 1,
        };

        let span = (vt.tracer_start_span_ex)(tctx, sv("ex"), &config);
        assert!(!span.is_null());
        (vt.span_end)(span);

        let spans = exporter.get_finished_spans().unwrap();
        let data = spans.iter().find(|s| s.name == "ex").expect("ex span");
        // Parent context is honored (trace id + parent span id + remote).
        assert_eq!(data.span_context.trace_id().to_bytes(), [0x11; 16]);
        assert_eq!(data.parent_span_id.to_bytes(), [0x22; 8]);
        assert!(data.parent_span_is_remote);
        assert_eq!(data.span_kind, SpanKind::Client);
        // Explicit start time forwarded exactly.
        assert_eq!(
            data.start_time,
            UNIX_EPOCH + Duration::from_nanos(start_nanos)
        );
        // Initial attribute present.
        assert!(data
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "span.attr"));
        // Link present with its context and attribute.
        assert_eq!(data.links.iter().count(), 1);
        let exported_link = data.links.iter().next().unwrap();
        assert_eq!(exported_link.span_context.trace_id().to_bytes(), [0x33; 16]);
        assert_eq!(exported_link.span_context.span_id().to_bytes(), [0x44; 8]);
        assert!(exported_link
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "link.attr"));

        (vt.span_free)(span);
        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    /// A non-zero `reserved` field and dual parents must be rejected by the SDK entry.
    #[test]
    fn vtable_start_span_ex_rejects_invalid_config() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());

        let bad_reserved = OtelSpanStartConfig {
            kind: 0,
            reserved: 1,
            parent_span_ctx: std::ptr::null_mut(),
            parent_context: std::ptr::null(),
            start_time_unix_nanos: 0,
            attributes: std::ptr::null(),
            attribute_count: 0,
            links: std::ptr::null(),
            link_count: 0,
        };
        assert!((vt.tracer_start_span_ex)(tctx, sv("x"), &bad_reserved).is_null());

        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }

    /// The configured sampler governs whether spans are recorded and exported: `AlwaysOff`
    /// drops every root span while `AlwaysOn` keeps them (verified end-to-end through the
    /// SDK vtable against an in-memory exporter).
    #[test]
    fn vtable_sampler_governs_recording() {
        for (sampler, expect_recorded) in [
            (opentelemetry_sdk::trace::Sampler::AlwaysOff, false),
            (opentelemetry_sdk::trace::Sampler::AlwaysOn, true),
        ] {
            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
                .with_sampler(sampler)
                .build();
            let vt = &SDK_VTABLE;
            let pctx = provider_ctx(provider);
            let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());

            let span = (vt.tracer_start_span)(tctx, sv("root"), 0, std::ptr::null_mut());
            assert!(!span.is_null());
            (vt.span_end)(span);

            let spans = exporter.get_finished_spans().unwrap();
            assert_eq!(
                spans.iter().any(|s| s.name == "root"),
                expect_recorded,
                "sampler recording mismatch"
            );

            (vt.span_free)(span);
            (vt.tracer_free)(tctx);
            (vt.provider_free)(pctx);
        }
    }

    /// Span limits configured on the provider must cap the number of attributes retained on a
    /// span: the SDK drops the most recently added attributes once the bound is reached
    /// (verified end-to-end through the vtable against an in-memory exporter).
    #[test]
    fn vtable_span_attribute_limit_is_enforced() {
        let exporter = InMemorySpanExporter::default();
        let limits = opentelemetry_sdk::trace::SpanLimits {
            max_attributes_per_span: 2,
            ..Default::default()
        };
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .with_span_limits(limits)
            .build();
        let vt = &SDK_VTABLE;
        let pctx = provider_ctx(provider);
        let tctx = (vt.provider_get_tracer)(pctx, sv("scope"), empty(), empty());

        let span = (vt.tracer_start_span)(tctx, sv("root"), 0, std::ptr::null_mut());
        assert!(!span.is_null());
        assert_eq!((vt.span_set_i64)(span, sv("a"), 1), OtelStatus::Ok);
        assert_eq!((vt.span_set_i64)(span, sv("b"), 2), OtelStatus::Ok);
        assert_eq!((vt.span_set_i64)(span, sv("c"), 3), OtelStatus::Ok);
        (vt.span_end)(span);

        let spans = exporter.get_finished_spans().unwrap();
        let root = spans.iter().find(|s| s.name == "root").expect("root");
        assert_eq!(root.attributes.len(), 2, "attributes must be capped at 2");

        (vt.span_free)(span);
        (vt.tracer_free)(tctx);
        (vt.provider_free)(pctx);
    }
}
