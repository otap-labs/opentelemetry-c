//! Trace API surface: tracer providers, tracers, and spans as opaque handles.
//!
//! Each handle stores a `*const OtelImplVtable` (NULL = the no-op default) plus an opaque
//! `*mut c_void` context that the SDK cdylib allocated. Every operation dispatches through
//! the vtable when backed, or is a safe no-op when not. No Rust SDK types cross this
//! boundary; the SDK frees its own contexts via the vtable `*_free` entries, and the API
//! frees only the API handle allocations.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use opentelemetry_c_abi::{
    trace_vtable_supports_context, trace_vtable_supports_span_context,
    trace_vtable_supports_span_start_ex, OtelAttributeType, OtelBool, OtelContextView,
    OtelHandleHeader, OtelImplVtable, OtelKeyValue, OtelSpanContextView, OtelSpanLinkView,
    OtelSpanStartConfig, OtelSpanStatusCode, OtelStringView, OTEL_HANDLE_KIND_SPAN,
    OTEL_HANDLE_KIND_SPAN_CONTEXT, OTEL_HANDLE_KIND_TRACER, OTEL_HANDLE_KIND_TRACER_PROVIDER,
};

use crate::context::current_data;
use crate::error::{clear_last_error, fail, set_last_error, OtelStatus};
use crate::global::{retain_global, GlobalRetain};
use crate::handle::{
    checked_ref, destroy, guard_ptr, guard_status, guard_unit, guard_value, into_raw,
    HasHandleHeader,
};

/// Backing selector for a provider handle.
pub(crate) enum ProviderInner {
    /// Resolve the process-global slot lazily on each tracer request.
    Global,
    /// A specific SDK-backed provider (owns its context; freed on destroy).
    Backed {
        vtable: *const OtelImplVtable,
        ctx: *mut c_void,
    },
}

/// Opaque tracer-provider handle (`otel_tracer_provider_t`).
#[repr(C)]
pub struct OtelTracerProvider {
    header: OtelHandleHeader,
    inner: ProviderInner,
}

impl OtelTracerProvider {
    pub(crate) fn new(inner: ProviderInner) -> Self {
        OtelTracerProvider {
            header: OtelHandleHeader::new(Self::KIND),
            inner,
        }
    }
}

impl HasHandleHeader for OtelTracerProvider {
    const KIND: u64 = OTEL_HANDLE_KIND_TRACER_PROVIDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Opaque tracer handle (`otel_tracer_t`). NULL `vtable` == no-op.
#[repr(C)]
pub struct OtelTracer {
    header: OtelHandleHeader,
    vtable: *const OtelImplVtable,
    ctx: *mut c_void,
}

impl HasHandleHeader for OtelTracer {
    const KIND: u64 = OTEL_HANDLE_KIND_TRACER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Opaque span handle (`otel_span_t`). NULL `vtable` == no-op.
#[repr(C)]
pub struct OtelSpan {
    header: OtelHandleHeader,
    vtable: *const OtelImplVtable,
    ctx: *mut c_void,
    ended: AtomicBool,
}

/// Immutable, API-owned trace-context snapshot (`otel_span_context_t`).
#[repr(C)]
pub struct OtelSpanContext {
    header: OtelHandleHeader,
    pub(crate) data: Arc<SpanContextData>,
}

#[derive(Clone)]
pub(crate) struct SpanContextData {
    pub(crate) trace_id: [u8; 16],
    pub(crate) span_id: [u8; 8],
    pub(crate) trace_flags: u8,
    pub(crate) is_remote: bool,
    pub(crate) trace_state: String,
}

impl SpanContextData {
    pub(crate) fn view(&self) -> OtelSpanContextView {
        OtelSpanContextView {
            trace_id: self.trace_id,
            span_id: self.span_id,
            trace_flags: self.trace_flags,
            reserved: [0; 3],
            is_remote: u32::from(self.is_remote),
            trace_state: OtelStringView {
                ptr: self.trace_state.as_ptr().cast(),
                len: self.trace_state.len(),
            },
        }
    }
}

impl HasHandleHeader for OtelSpanContext {
    const KIND: u64 = OTEL_HANDLE_KIND_SPAN_CONTEXT;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<OtelSpanContext>;
};

impl OtelSpanContext {
    pub(crate) fn from_parts(
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        is_remote: bool,
        trace_state: String,
    ) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            data: Arc::new(SpanContextData {
                trace_id,
                span_id,
                trace_flags,
                is_remote,
                trace_state,
            }),
        }
    }

    pub(crate) fn from_data(data: Arc<SpanContextData>) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            data,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.data.trace_id != [0; 16] && self.data.span_id != [0; 8]
    }

    pub(crate) fn view(&self) -> OtelSpanContextView {
        self.data.view()
    }
}

impl OtelSpan {
    fn end(&self) {
        if !self.ended.swap(true, Ordering::AcqRel) && !self.vtable.is_null() {
            // SAFETY: `vtable` is a live registered vtable; `ctx` its span context.
            unsafe { ((*self.vtable).span_end)(self.ctx) };
        }
    }

    fn end_and_free_ctx(&self) {
        if self.vtable.is_null() {
            return;
        }
        let ended = self.ended.swap(true, Ordering::AcqRel);
        // SAFETY: `vtable` is live; `ctx` its span context. Free ends it if needed.
        unsafe {
            if !ended {
                ((*self.vtable).span_end)(self.ctx);
            }
            ((*self.vtable).span_free)(self.ctx);
        }
    }
}

impl HasHandleHeader for OtelSpan {
    const KIND: u64 = OTEL_HANDLE_KIND_SPAN;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

// SAFETY: provider and tracer handles are documented as safe to share across threads. Their
// raw pointers reference SDK objects that are `Send + Sync` (Arc-backed provider / BoxedTracer)
// and whose vtable functions take shared access, so concurrent use is sound. (Span handles
// carry a single-thread contract and are intentionally not marked Sync.)
unsafe impl Send for OtelTracerProvider {}
unsafe impl Sync for OtelTracerProvider {}
unsafe impl Send for OtelTracer {}
unsafe impl Sync for OtelTracer {}

// The C contract documents provider/tracer handles as concurrency-safe; assert `Sync` at
// compile time so a future non-`Sync` field breaks the build.
const _: () = {
    fn assert_sync<T: Sync>() {}
    let _ = assert_sync::<OtelTracerProvider>;
    let _ = assert_sync::<OtelTracer>;
};

/// Options for [`otel_tracer_start_span`]. NULL selects `Internal` kind and no parent.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanStartOptions {
    /// Span kind, an [`opentelemetry_c_abi::OtelSpanKind`] value. Unknown => `Internal`.
    pub kind: u32,
    /// Optional parent span; NULL => root span. Borrowed for the call only.
    pub parent: *const OtelSpan,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelSpanStartOptions>() == 16);
    assert!(std::mem::align_of::<OtelSpanStartOptions>() == 8);
};

/// A span link: an immutable parent context plus optional link attributes.
///
/// All pointers are borrowed for the duration of the [`otel_tracer_start_span_ex`] call only.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanLink {
    /// Linked span context. Must be a live [`OtelSpanContext`] handle; a link with a NULL or
    /// invalid context is rejected.
    pub context: *const OtelSpanContext,
    /// Optional link attributes, or NULL when `attribute_count == 0`. Borrowed for the call.
    pub attributes: *const OtelKeyValue,
    pub attribute_count: usize,
}

/// Versioned options for [`otel_tracer_start_span_ex`].
///
/// The first field is `struct_size`, which the caller sets to `sizeof` of this struct as it
/// was compiled. The API reads only the fields covered by `struct_size`, so a newer header may
/// append fields without breaking an older implementation and vice versa.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanStartOptionsEx {
    /// Size in bytes of this struct as seen by the caller. Must cover at least through
    /// `start_time_unix_nanos`.
    pub struct_size: usize,
    /// Span kind, an [`opentelemetry_c_abi::OtelSpanKind`] value. Unknown => `Internal`.
    pub kind: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// Optional live local parent span; NULL => no live parent. Mutually exclusive with
    /// `parent_context`.
    pub parent: *const OtelSpan,
    /// Optional immutable parent context; NULL => no context parent. Mutually exclusive with
    /// `parent`.
    pub parent_context: *const OtelSpanContext,
    /// Explicit start time in Unix nanoseconds; 0 => unset (the implementation assigns now).
    pub start_time_unix_nanos: u64,
    /// Optional initial span attributes, or NULL when `attribute_count == 0`. Borrowed.
    pub attributes: *const OtelKeyValue,
    pub attribute_count: usize,
    /// Optional span links, or NULL when `link_count == 0`. Borrowed for the call.
    pub links: *const OtelSpanLink,
    pub link_count: usize,
    /// Parenting policy. 0 = explicit fields/current behavior, 1 = ambient C context,
    /// 2 = force root. Read only when `struct_size` covers this appended field.
    pub parent_mode: u32,
    /// Reserved; must be zero when `parent_mode` is present.
    pub reserved2: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelSpanStartOptionsEx>() == 80);
    assert!(std::mem::align_of::<OtelSpanStartOptionsEx>() == 8);
};

/// Minimum `struct_size` accepted by [`otel_tracer_start_span_ex`]: through
/// `start_time_unix_nanos`. Attributes and links are read only when `struct_size` covers them.
const OTEL_SPAN_START_OPTIONS_EX_REQUIRED_SIZE: usize =
    std::mem::offset_of!(OtelSpanStartOptionsEx, attributes);
const OTEL_SPAN_START_OPTIONS_EX_V1_SIZE: usize =
    std::mem::offset_of!(OtelSpanStartOptionsEx, parent_mode);

pub const OTEL_PARENT_EXPLICIT: u32 = 0;
pub const OTEL_PARENT_AMBIENT: u32 = 1;
pub const OTEL_PARENT_ROOT: u32 = 2;

/// Upper bound on span links accepted in a single start-span call.
const OTEL_SPAN_START_MAX_LINKS: usize = 1 << 16;

fn new_tracer(vtable: *const OtelImplVtable, ctx: *mut c_void) -> *mut OtelTracer {
    into_raw(OtelTracer {
        header: OtelHandleHeader::new(OtelTracer::KIND),
        vtable,
        ctx,
    })
}

fn new_span(vtable: *const OtelImplVtable, ctx: *mut c_void) -> *mut OtelSpan {
    into_raw(OtelSpan {
        header: OtelHandleHeader::new(OtelSpan::KIND),
        vtable,
        ctx,
        ended: AtomicBool::new(false),
    })
}

fn vtable_has_span_context(vtable: *const OtelImplVtable) -> bool {
    // SAFETY: callers pass a live registered trace vtable, whose stable header is readable.
    unsafe { trace_vtable_supports_span_context(vtable) }
}

fn vtable_has_span_start_ex(vtable: *const OtelImplVtable) -> bool {
    // SAFETY: callers pass a live registered trace vtable, whose stable header is readable.
    unsafe { trace_vtable_supports_span_start_ex(vtable) }
}

fn vtable_has_context(vtable: *const OtelImplVtable) -> bool {
    unsafe { trace_vtable_supports_context(vtable) }
}

/// Whether this tracer's installed implementation supports ambient/general context parenting.
///
/// # Safety
/// `tracer` must be a live handle not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_supports_context(tracer: *const OtelTracer) -> OtelBool {
    guard_value(0, || match unsafe { checked_ref::<OtelTracer>(tracer) } {
        Some(tracer) if !tracer.vtable.is_null() && vtable_has_context(tracer.vtable) => 1,
        _ => 0,
    })
}

struct SnapshotReceiver {
    context: Option<OtelSpanContext>,
}

extern "C" fn receive_span_context(
    user_data: *mut c_void,
    view: *const OtelSpanContextView,
) -> OtelStatus {
    guard_status(|| {
        if user_data.is_null() || view.is_null() {
            set_last_error("span context visitor received a NULL pointer");
            return OtelStatus::InternalError;
        }
        // SAFETY: both pointers are supplied by the synchronous vtable call below.
        let receiver = unsafe { &mut *(user_data as *mut SnapshotReceiver) };
        let view = unsafe { &*view };
        if view.reserved != [0; 3] || view.trace_id == [0; 16] || view.span_id == [0; 8] {
            set_last_error("SDK returned an invalid span context");
            return OtelStatus::InternalError;
        }
        let value = match unsafe { view.trace_state.as_str() } {
            Ok(value) => value,
            Err(error) => {
                set_last_error(error.message);
                return error.status;
            }
        };
        let mut trace_state = String::new();
        if trace_state.try_reserve_exact(value.len()).is_err() {
            return fail(
                OtelStatus::InternalError,
                "failed to allocate span context trace state",
            );
        }
        trace_state.push_str(value);
        receiver.context = Some(OtelSpanContext::from_parts(
            view.trace_id,
            view.span_id,
            view.trace_flags,
            view.is_remote != 0,
            trace_state,
        ));
        OtelStatus::Ok
    })
}

/// Copy the immutable context of a live SDK-backed span into an API-owned handle.
///
/// # Safety
/// `span` must be live and not destroyed concurrently. `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn otel_span_get_context(
    span: *const OtelSpan,
    out: *mut *mut OtelSpanContext,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "span context output pointer is NULL",
            );
        }
        unsafe { *out = std::ptr::null_mut() };
        let span = match unsafe { checked_ref(span) } {
            Some(span) => span,
            None => return OtelStatus::InvalidArgument,
        };
        if span.vtable.is_null() {
            return fail(
                OtelStatus::InvalidConfig,
                "a no-op span has no valid span context",
            );
        }
        let vtable = span.vtable;
        if !vtable_has_span_context(vtable) {
            return fail(
                OtelStatus::InvalidConfig,
                "installed trace implementation does not support span-context snapshots",
            );
        }
        let mut receiver = SnapshotReceiver { context: None };
        // SAFETY: the feature-size check above proves the complete current vtable is readable.
        let Some(vtable) = (unsafe { vtable.as_ref() }) else {
            return fail(
                OtelStatus::InvalidConfig,
                "trace implementation vtable is NULL",
            );
        };
        let visit = vtable.span_context_visit;
        let status = visit(
            span.ctx,
            Some(receive_span_context),
            (&mut receiver as *mut SnapshotReceiver).cast(),
        );
        if status != OtelStatus::Ok {
            return status;
        }
        let Some(context) = receiver.context else {
            return fail(
                OtelStatus::InternalError,
                "trace implementation did not return a span context",
            );
        };
        unsafe { *out = into_raw(context) };
        OtelStatus::Ok
    })
}

/// Clone an immutable span-context snapshot.
///
/// # Safety
/// `context` must be a live context handle not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_clone(
    context: *const OtelSpanContext,
) -> *mut OtelSpanContext {
    guard_ptr(|| {
        clear_last_error();
        let context = match unsafe { checked_ref(context) } {
            Some(context) => context,
            None => return std::ptr::null_mut(),
        };
        let mut trace_state = String::new();
        if trace_state
            .try_reserve_exact(context.data.trace_state.len())
            .is_err()
        {
            fail(
                OtelStatus::InternalError,
                "failed to allocate cloned span context trace state",
            );
            return std::ptr::null_mut();
        }
        trace_state.push_str(&context.data.trace_state);
        into_raw(OtelSpanContext::from_parts(
            context.data.trace_id,
            context.data.span_id,
            context.data.trace_flags,
            context.data.is_remote,
            trace_state,
        ))
    })
}

/// Destroy an immutable span-context snapshot.
///
/// # Safety
/// `context` must be NULL or a live owned handle not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_destroy(context: *mut OtelSpanContext) {
    guard_unit(|| unsafe { destroy(context) });
}

/// Maximum tracestate byte length accepted by [`otel_span_context_create`]. Bounds the copy
/// for a caller-constructed context; the W3C recommendation keeps tracestate far below this.
const OTEL_SPAN_CONTEXT_MAX_TRACESTATE: usize = 32 * 1024;

/// Whether a span context is valid: a non-zero 16-byte trace ID **and** non-zero 8-byte span ID.
///
/// Returns `OTEL_FALSE` (0) for a NULL or wrong-kind handle. Never fails.
///
/// # Safety
/// `context` must be NULL or a live context handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_is_valid(context: *const OtelSpanContext) -> OtelBool {
    guard_value(0, || {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => u32::from(c.is_valid()),
            None => 0,
        }
    })
}

/// Whether a span context was extracted from a remote parent.
///
/// Returns `OTEL_FALSE` (0) for a NULL or wrong-kind handle. Never fails.
///
/// # Safety
/// `context` must be NULL or a live context handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_is_remote(context: *const OtelSpanContext) -> OtelBool {
    guard_value(0, || {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => u32::from(c.data.is_remote),
            None => 0,
        }
    })
}

/// Copy the 16-byte, W3C big-endian trace ID into `out`.
///
/// `out` must point to at least 16 writable bytes. Zero-fills nothing on failure.
///
/// # Safety
/// `context` must be NULL or a live context handle; `out` must be writable for 16 bytes.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_trace_id(
    context: *const OtelSpanContext,
    out: *mut u8,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "trace id output buffer is NULL",
            );
        }
        // SAFETY: forwarded to the caller's contract.
        let context = match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => c,
            None => return OtelStatus::InvalidArgument,
        };
        // SAFETY: caller guarantees `out` is writable for at least 16 bytes; the source is a
        // fixed 16-byte array, and the two regions do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(context.data.trace_id.as_ptr(), out, 16) };
        OtelStatus::Ok
    })
}

/// Copy the 8-byte, W3C big-endian span ID into `out`.
///
/// `out` must point to at least 8 writable bytes.
///
/// # Safety
/// `context` must be NULL or a live context handle; `out` must be writable for 8 bytes.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_span_id(
    context: *const OtelSpanContext,
    out: *mut u8,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "span id output buffer is NULL");
        }
        // SAFETY: forwarded to the caller's contract.
        let context = match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => c,
            None => return OtelStatus::InvalidArgument,
        };
        // SAFETY: caller guarantees `out` is writable for at least 8 bytes; the source is a
        // fixed 8-byte array, and the two regions do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(context.data.span_id.as_ptr(), out, 8) };
        OtelStatus::Ok
    })
}

/// Write the opaque `uint8_t` trace flags into `*out`.
///
/// All 8 bits are preserved verbatim, including unknown/reserved bits; interpret the
/// `sampled` bit as `0x01`.
///
/// # Safety
/// `context` must be NULL or a live context handle; `out` must be writable for 1 byte.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_trace_flags(
    context: *const OtelSpanContext,
    out: *mut u8,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "trace flags output pointer is NULL",
            );
        }
        // SAFETY: forwarded to the caller's contract.
        let context = match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) => c,
            None => return OtelStatus::InvalidArgument,
        };
        // SAFETY: caller guarantees `out` is writable for 1 byte.
        unsafe { *out = context.data.trace_flags };
        OtelStatus::Ok
    })
}

/// Borrow the tracestate as a UTF-8 view. The returned bytes are owned by `context` and remain
/// valid until it is destroyed; copy them to retain beyond that. An empty tracestate (or a
/// NULL/wrong-kind handle) yields an empty view (`ptr == NULL`, `len == 0`).
///
/// # Safety
/// `context` must be NULL or a live context handle, not destroyed while the view is in use.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_tracestate(
    context: *const OtelSpanContext,
) -> OtelStringView {
    guard_value(OtelStringView::empty(), || {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        match unsafe { checked_ref::<OtelSpanContext>(context) } {
            Some(c) if !c.data.trace_state.is_empty() => OtelStringView {
                ptr: c.data.trace_state.as_ptr().cast(),
                len: c.data.trace_state.len(),
            },
            _ => OtelStringView::empty(),
        }
    })
}

/// Construct an owned immutable span context from raw parts.
///
/// `trace_id` points to 16 bytes and `span_id` to 8 bytes, both in W3C big-endian order.
/// `trace_flags` is stored opaquely. `trace_state` is a borrowed UTF-8 view copied before
/// return; pass an empty view for none. All-zero trace/span IDs are rejected. Returns NULL on
/// invalid arguments or allocation failure with the last-error set. Release with
/// [`otel_span_context_destroy`].
///
/// tracestate is validated as UTF-8 and bounded in length here; full W3C `tracestate` grammar
/// validation is performed by the propagation extract path, not by raw construction.
///
/// # Safety
/// `trace_id`/`span_id` must be readable for 16/8 bytes; `trace_state` must satisfy the
/// `otel_string_view_t` contract.
#[no_mangle]
pub unsafe extern "C" fn otel_span_context_create(
    trace_id: *const u8,
    span_id: *const u8,
    trace_flags: u8,
    is_remote: OtelBool,
    trace_state: OtelStringView,
) -> *mut OtelSpanContext {
    guard_ptr(|| {
        clear_last_error();
        if trace_id.is_null() || span_id.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "trace id / span id pointer is NULL",
            );
            return std::ptr::null_mut();
        }
        let mut tid = [0u8; 16];
        let mut sid = [0u8; 8];
        // SAFETY: caller guarantees `trace_id`/`span_id` are readable for 16/8 bytes; the
        // destinations are distinct local arrays.
        unsafe {
            std::ptr::copy_nonoverlapping(trace_id, tid.as_mut_ptr(), 16);
            std::ptr::copy_nonoverlapping(span_id, sid.as_mut_ptr(), 8);
        }
        if tid == [0u8; 16] || sid == [0u8; 8] {
            fail(
                OtelStatus::InvalidArgument,
                "span context requires a non-zero trace id and span id",
            );
            return std::ptr::null_mut();
        }
        if trace_state.len > OTEL_SPAN_CONTEXT_MAX_TRACESTATE {
            fail(
                OtelStatus::InvalidArgument,
                "tracestate exceeds the maximum supported length",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded to the caller's contract for the string view.
        let value = match unsafe { trace_state.as_str() } {
            Ok(v) => v,
            Err(error) => {
                set_last_error(error.message);
                return std::ptr::null_mut();
            }
        };
        let mut owned = String::new();
        if owned.try_reserve_exact(value.len()).is_err() {
            fail(
                OtelStatus::InternalError,
                "failed to allocate span context trace state",
            );
            return std::ptr::null_mut();
        }
        owned.push_str(value);
        into_raw(OtelSpanContext::from_parts(
            tid,
            sid,
            trace_flags,
            is_remote != 0,
            owned,
        ))
    })
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Obtain a tracer from a provider.
///
/// - Invalid provider handle: returns NULL.
/// - No SDK installed (unbacked global provider): returns a valid **no-op** tracer.
/// - A backed implementation whose tracer creation fails (e.g. malformed string view or
///   allocation failure): returns NULL with the last-error set — **not** a no-op tracer.
///
/// # Safety
/// `provider` must satisfy the handle contract; the string views must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_provider_get_tracer(
    provider: *const OtelTracerProvider,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut OtelTracer {
    guard_ptr(|| {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        let provider = match unsafe { checked_ref(provider) } {
            Some(p) => p,
            None => return std::ptr::null_mut(),
        };
        // Resolve the backing implementation. For the process-global provider we retain an
        // OWNED reference to its context (under the global read lock) so it cannot be freed
        // by a concurrent replacement while we use it; `owned` marks that we must release it.
        let (vtable, ctx, owned) = match &provider.inner {
            ProviderInner::Global => match retain_global() {
                // No SDK installed: a genuine unbacked provider — a valid no-op tracer.
                GlobalRetain::NoProvider => {
                    return new_tracer(std::ptr::null(), std::ptr::null_mut());
                }
                // A provider IS installed but retaining it failed; `retain_global` left the
                // last-error set. Surface the failure as NULL — NOT a no-op tracer.
                GlobalRetain::RetainFailed => return std::ptr::null_mut(),
                GlobalRetain::Retained { vtable, ctx } => (vtable, ctx, true),
            },
            ProviderInner::Backed { vtable, ctx } => (*vtable, *ctx, false),
        };
        if vtable.is_null() {
            // Defensive: a `Backed` provider is always created with a non-NULL vtable
            // (`otel_api_provider_new` rejects NULL), and `Retained` always carries the live
            // slot vtable. Treat any unexpected NULL as an unbacked no-op rather than
            // dereferencing it.
            return new_tracer(std::ptr::null(), std::ptr::null_mut());
        }
        // SAFETY: `vtable` is a live registered vtable; `ctx` is a valid provider context (an
        // owned retained reference when `owned`, else the Backed handle's own context).
        let tracer_ctx = unsafe { ((*vtable).provider_get_tracer)(ctx, name, version, schema_url) };
        if owned {
            // SAFETY: release the retained global reference exactly once, regardless of the
            // get_tracer result. `provider_free` does not touch the last-error slot.
            unsafe { ((*vtable).provider_free)(ctx) };
        }
        if tracer_ctx.is_null() {
            // A REAL backed/global implementation was asked and failed (malformed view,
            // allocation failure, or a guarded vtable panic); it left the last-error set.
            // Surface the failure as NULL — do NOT clear the error or degrade to a no-op.
            return std::ptr::null_mut();
        }
        new_tracer(vtable, tracer_ctx)
    })
}

/// Destroy a tracer-provider handle (no-op on NULL). Frees a backed provider's context.
///
/// # Safety
/// `provider` must be NULL or a live provider handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_provider_destroy(provider: *mut OtelTracerProvider) {
    guard_unit(|| {
        if let Some(p) = unsafe { checked_ref::<OtelTracerProvider>(provider) } {
            // Match by reference and copy the raw pointer fields locally — `ProviderInner`
            // is not `Copy`, so this must not move `inner` out of the shared borrow. A
            // `Global` provider owns no context and needs no free.
            if let ProviderInner::Backed { vtable, ctx } = &p.inner {
                let (vtable, ctx) = (*vtable, *ctx);
                if !vtable.is_null() {
                    // SAFETY: `vtable` is live; free the owned provider context exactly once.
                    unsafe { ((*vtable).provider_free)(ctx) };
                }
            }
        }
        // SAFETY: forwarded to the caller's contract.
        unsafe { destroy(provider) };
    });
}

// ---------------------------------------------------------------------------
// Tracer
// ---------------------------------------------------------------------------

/// Start a new span.
///
/// - Invalid tracer handle, malformed `options`, or a non-NULL but invalid `parent`: NULL.
/// - Unbacked (no-op) tracer: returns a valid **no-op** span.
/// - A backed tracer whose span creation fails (e.g. malformed name or allocation failure):
///   returns NULL with the last-error set — **not** a no-op span.
///
/// A parent span produced by a *different* implementation (its vtable differs from this
/// tracer's) is treated as **no parent**, so the new span is a root span. See `trace.h`.
///
/// # Safety
/// `tracer` must satisfy the handle contract; `options` (if non-NULL) must point to a valid
/// [`OtelSpanStartOptions`] whose `parent` is NULL or a live span handle; `name` valid.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_start_span(
    tracer: *const OtelTracer,
    name: OtelStringView,
    options: *const OtelSpanStartOptions,
) -> *mut OtelSpan {
    guard_ptr(|| {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        let tracer = match unsafe { checked_ref(tracer) } {
            Some(t) => t,
            None => return std::ptr::null_mut(),
        };
        // Copy the validated vtable into a local so we don't repeatedly read the handle's raw
        // pointer field after the null check (also clears static-analysis warnings about
        // dereferencing a raw pointer loaded from the handle).
        let vtable = tracer.vtable;
        if vtable.is_null() {
            // Unbacked (no-op) tracer: a valid no-op span, as the spec expects.
            return new_span(std::ptr::null(), std::ptr::null_mut());
        }

        let mut kind: u32 = 0;
        let mut parent_ctx: *mut c_void = std::ptr::null_mut();
        if !options.is_null() {
            // SAFETY: caller guarantees a valid options pointer when non-NULL.
            let options = unsafe { &*options };
            kind = options.kind;
            if !options.parent.is_null() {
                // SAFETY: caller guarantees parent is NULL or a live span handle.
                match unsafe { checked_ref::<OtelSpan>(options.parent) } {
                    // Only pass the parent context if it belongs to the SAME implementation
                    // (same vtable); a parent from a different implementation is treated as
                    // no parent, so the new span is a root span (documented in trace.h).
                    Some(parent) if parent.vtable == vtable => parent_ctx = parent.ctx,
                    Some(_) => {}
                    None => return std::ptr::null_mut(),
                }
            }
        }

        // SAFETY: `vtable` is live; `tracer.ctx` its tracer context.
        let span_ctx = unsafe { ((*vtable).tracer_start_span)(tracer.ctx, name, kind, parent_ctx) };
        if span_ctx.is_null() {
            // A REAL backed tracer was asked and failed (malformed name, allocation failure,
            // or a guarded vtable panic); it left the last-error set. Surface the failure as
            // NULL — do NOT clear the error or degrade to a no-op span.
            return std::ptr::null_mut();
        }
        new_span(vtable, span_ctx)
    })
}

/// Start a span using an immutable implementation-neutral parent context.
///
/// # Safety
/// All handles must be live and not destroyed concurrently. `name` and `options`, when
/// present, must satisfy their ordinary public contracts.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_start_span_with_context(
    tracer: *const OtelTracer,
    name: OtelStringView,
    options: *const OtelSpanStartOptions,
    parent: *const OtelSpanContext,
) -> *mut OtelSpan {
    guard_ptr(|| {
        clear_last_error();
        let tracer = match unsafe { checked_ref(tracer) } {
            Some(tracer) => tracer,
            None => return std::ptr::null_mut(),
        };
        let parent = match unsafe { checked_ref(parent) } {
            Some(parent) if parent.is_valid() => parent,
            Some(_) => {
                fail(
                    OtelStatus::InvalidArgument,
                    "parent span context is invalid",
                );
                return std::ptr::null_mut();
            }
            None => return std::ptr::null_mut(),
        };
        let mut kind = 0;
        if !options.is_null() {
            let options = unsafe { &*options };
            if !options.parent.is_null() {
                fail(
                    OtelStatus::InvalidArgument,
                    "parent span handle and parent span context are mutually exclusive",
                );
                return std::ptr::null_mut();
            }
            kind = options.kind;
        }
        if tracer.vtable.is_null() {
            return new_span(std::ptr::null(), std::ptr::null_mut());
        }
        let vtable = tracer.vtable;
        if !vtable_has_span_context(vtable) {
            fail(
                OtelStatus::InvalidConfig,
                "installed trace implementation does not support context parenting",
            );
            return std::ptr::null_mut();
        }
        let view = parent.view();
        // SAFETY: the feature-size check above proves the complete current vtable is readable.
        let Some(vtable) = (unsafe { vtable.as_ref() }) else {
            fail(
                OtelStatus::InvalidConfig,
                "trace implementation vtable is NULL",
            );
            return std::ptr::null_mut();
        };
        let start_span = vtable.tracer_start_span_with_context;
        let span_ctx = start_span(tracer.ctx, name, kind, &view);
        if span_ctx.is_null() {
            return std::ptr::null_mut();
        }
        new_span(tracer.vtable, span_ctx)
    })
}

/// Start a span from a versioned [`OtelSpanStartOptionsEx`] descriptor supporting links, an
/// explicit start timestamp, initial attributes, and a single parenting source.
///
/// `options` must be non-NULL. Its `struct_size` must be at least
/// `OTEL_SPAN_START_OPTIONS_EX_REQUIRED_SIZE`; attributes and links are read only when
/// `struct_size` covers those fields, so older/newer headers interoperate. `parent` and
/// `parent_context` are mutually exclusive. A live parent from a *different* implementation is
/// treated as no parent (root span), matching [`otel_tracer_start_span`].
///
/// A no-op (unbacked) tracer returns a valid no-op span. A backed tracer whose installed
/// implementation predates extended span-start support returns NULL with
/// [`OtelStatus::InvalidConfig`].
///
/// # Safety
/// `tracer` must satisfy the handle contract. `options` must point to a valid
/// [`OtelSpanStartOptionsEx`] with `struct_size` bytes readable; every borrowed pointer
/// (parent, parent_context, attributes, links and each link's context/attributes) must be live
/// for the duration of the call. `name` must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_start_span_ex(
    tracer: *const OtelTracer,
    name: OtelStringView,
    options: *const OtelSpanStartOptionsEx,
) -> *mut OtelSpan {
    guard_ptr(|| {
        clear_last_error();
        let tracer = match unsafe { checked_ref(tracer) } {
            Some(t) => t,
            None => return std::ptr::null_mut(),
        };
        if options.is_null() {
            fail(OtelStatus::InvalidArgument, "options must not be NULL");
            return std::ptr::null_mut();
        }
        // A genuinely older caller may pass a struct shorter than the current
        // `OtelSpanStartOptionsEx`, so we must never form a reference to the full struct. Read
        // the stable `struct_size` prefix first, then read each covered field through its own
        // raw address, mirroring the Metrics/Logs versioned-struct handling.
        // SAFETY: `struct_size` is the first field and is always present for non-NULL options.
        let struct_size = unsafe { options.cast::<usize>().read() };
        if struct_size < OTEL_SPAN_START_OPTIONS_EX_REQUIRED_SIZE {
            fail(
                OtelStatus::InvalidConfig,
                "options struct_size is smaller than the required minimum",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: `struct_size >= REQUIRED_SIZE` proves the prefix through
        // `start_time_unix_nanos` is present; take each field's address without dereferencing
        // the whole (possibly truncated) struct.
        let kind = unsafe { std::ptr::addr_of!((*options).kind).read() };
        let reserved = unsafe { std::ptr::addr_of!((*options).reserved).read() };
        let parent = unsafe { std::ptr::addr_of!((*options).parent).read() };
        let parent_context = unsafe { std::ptr::addr_of!((*options).parent_context).read() };
        let start_time_unix_nanos =
            unsafe { std::ptr::addr_of!((*options).start_time_unix_nanos).read() };
        if reserved != 0 {
            fail(
                OtelStatus::InvalidArgument,
                "reserved field in span-start options must be zero",
            );
            return std::ptr::null_mut();
        }
        if !parent.is_null() && !parent_context.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "parent span handle and parent span context are mutually exclusive",
            );
            return std::ptr::null_mut();
        }

        // Fields present only when `struct_size` covers them fully (through the end of the
        // field). Offsets are computed from the struct layout so newer/older headers agree.
        let has_attributes = struct_size >= std::mem::offset_of!(OtelSpanStartOptionsEx, links);
        let has_links = struct_size >= OTEL_SPAN_START_OPTIONS_EX_V1_SIZE;
        let has_parent_mode = struct_size >= std::mem::size_of::<OtelSpanStartOptionsEx>();
        // SAFETY: each tail field is read only when `struct_size` proves it is fully present.
        let (attributes, attribute_count) = if has_attributes {
            unsafe {
                (
                    std::ptr::addr_of!((*options).attributes).read(),
                    std::ptr::addr_of!((*options).attribute_count).read(),
                )
            }
        } else {
            (std::ptr::null(), 0)
        };
        let (links, link_count) = if has_links {
            unsafe {
                (
                    std::ptr::addr_of!((*options).links).read(),
                    std::ptr::addr_of!((*options).link_count).read(),
                )
            }
        } else {
            (std::ptr::null(), 0)
        };
        let (parent_mode, reserved2) = if has_parent_mode {
            unsafe {
                (
                    std::ptr::addr_of!((*options).parent_mode).read(),
                    std::ptr::addr_of!((*options).reserved2).read(),
                )
            }
        } else {
            (OTEL_PARENT_EXPLICIT, 0)
        };
        if reserved2 != 0 || parent_mode > OTEL_PARENT_ROOT {
            fail(
                OtelStatus::InvalidArgument,
                "parent mode or its reserved field is invalid",
            );
            return std::ptr::null_mut();
        }
        if parent_mode != OTEL_PARENT_EXPLICIT && (!parent.is_null() || !parent_context.is_null()) {
            fail(
                OtelStatus::InvalidArgument,
                "ambient/root parent mode cannot include an explicit parent",
            );
            return std::ptr::null_mut();
        }

        if attributes.is_null() && attribute_count != 0 {
            fail(
                OtelStatus::InvalidArgument,
                "attribute array is NULL with a non-zero count",
            );
            return std::ptr::null_mut();
        }
        if links.is_null() && link_count != 0 {
            fail(
                OtelStatus::InvalidArgument,
                "link array is NULL with a non-zero count",
            );
            return std::ptr::null_mut();
        }
        if link_count > OTEL_SPAN_START_MAX_LINKS {
            fail(
                OtelStatus::InvalidArgument,
                "link count exceeds the maximum supported value",
            );
            return std::ptr::null_mut();
        }

        let vtable = tracer.vtable;
        if vtable.is_null() {
            // Unbacked (no-op) tracer: a valid no-op span.
            return new_span(std::ptr::null(), std::ptr::null_mut());
        }
        if !vtable_has_span_start_ex(vtable) {
            fail(
                OtelStatus::InvalidConfig,
                "installed trace implementation does not support extended span start",
            );
            return std::ptr::null_mut();
        }

        // Resolve the single parenting source.
        let mut parent_span_ctx: *mut c_void = std::ptr::null_mut();
        let mut parent_view_storage: Option<OtelSpanContextView> = None;
        if !parent.is_null() {
            // SAFETY: caller guarantees a live span handle when non-NULL.
            match unsafe { checked_ref::<OtelSpan>(parent) } {
                // Only a parent from the SAME implementation contributes a live context.
                Some(parent) if parent.vtable == vtable => parent_span_ctx = parent.ctx,
                Some(_) => {}
                None => return std::ptr::null_mut(),
            }
        } else if !parent_context.is_null() {
            // SAFETY: caller guarantees a live span-context handle when non-NULL.
            match unsafe { checked_ref::<OtelSpanContext>(parent_context) } {
                Some(parent) if parent.is_valid() => parent_view_storage = Some(parent.view()),
                Some(_) => {
                    fail(
                        OtelStatus::InvalidArgument,
                        "parent span context is invalid",
                    );
                    return std::ptr::null_mut();
                }
                None => return std::ptr::null_mut(),
            }
        }

        // Build borrowed link views. Each link context handle is validated and its view borrows
        // the handle's tracestate string, which stays live for the duration of this call.
        let mut link_views: Vec<OtelSpanLinkView> = Vec::new();
        if link_count != 0 {
            if link_views.try_reserve(link_count).is_err() {
                fail(
                    OtelStatus::InternalError,
                    "failed to allocate space for span links",
                );
                return std::ptr::null_mut();
            }
            // SAFETY: `links` is non-NULL with `link_count` valid elements (checked above).
            let link_slice = unsafe { std::slice::from_raw_parts(links, link_count) };
            for link in link_slice {
                // SAFETY: each link context must be a live span-context handle.
                let ctx = match unsafe { checked_ref::<OtelSpanContext>(link.context) } {
                    Some(ctx) if ctx.is_valid() => ctx,
                    Some(_) => {
                        fail(OtelStatus::InvalidArgument, "link span context is invalid");
                        return std::ptr::null_mut();
                    }
                    None => {
                        fail(
                            OtelStatus::InvalidArgument,
                            "link span context is NULL or not a span-context handle",
                        );
                        return std::ptr::null_mut();
                    }
                };
                if link.attributes.is_null() && link.attribute_count != 0 {
                    fail(
                        OtelStatus::InvalidArgument,
                        "link attribute array is NULL with a non-zero count",
                    );
                    return std::ptr::null_mut();
                }
                link_views.push(OtelSpanLinkView {
                    context: ctx.view(),
                    attributes: link.attributes,
                    attribute_count: link.attribute_count,
                });
            }
        }

        let parent_context_ptr = parent_view_storage
            .as_ref()
            .map_or(std::ptr::null(), |v| v as *const OtelSpanContextView);
        let links_ptr = if link_views.is_empty() {
            std::ptr::null()
        } else {
            link_views.as_ptr()
        };

        let config = OtelSpanStartConfig {
            kind,
            reserved: 0,
            parent_span_ctx,
            parent_context: parent_context_ptr,
            start_time_unix_nanos,
            attributes,
            attribute_count,
            links: links_ptr,
            link_count: link_views.len(),
        };

        // SAFETY: the feature-size check above proves the extended entry is readable.
        let ambient_data = if parent_mode == OTEL_PARENT_AMBIENT {
            current_data()
        } else {
            None
        };
        let ambient_span_view = ambient_data
            .as_ref()
            .and_then(|data| data.span_context.as_ref())
            .map(|data| data.view());
        let ambient_view = OtelContextView {
            struct_size: std::mem::size_of::<OtelContextView>(),
            span_context: ambient_span_view
                .as_ref()
                .map_or(std::ptr::null(), |view| view),
            flags: ambient_data.as_ref().map_or(0, |data| data.flags),
            reserved: 0,
        };
        let span_ctx = if parent_mode == OTEL_PARENT_AMBIENT {
            if !vtable_has_context(vtable) {
                fail(
                    OtelStatus::InvalidConfig,
                    "installed trace implementation does not support ambient context",
                );
                return std::ptr::null_mut();
            }
            let start =
                unsafe { std::ptr::addr_of!((*vtable).tracer_start_span_ex_with_context).read() };
            start(tracer.ctx, name, &config, &ambient_view)
        } else {
            let start = unsafe { std::ptr::addr_of!((*vtable).tracer_start_span_ex).read() };
            start(tracer.ctx, name, &config)
        };
        // `link_views` and `parent_view_storage` are owned locals borrowed by `config` through
        // raw pointers; they remain live until the end of this scope, i.e. across the call.
        if span_ctx.is_null() {
            return std::ptr::null_mut();
        }
        new_span(vtable, span_ctx)
    })
}

/// Destroy a tracer handle (no-op on NULL). Frees a backed tracer's context.
///
/// # Safety
/// `tracer` must be NULL or a live tracer handle, not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_tracer_destroy(tracer: *mut OtelTracer) {
    guard_unit(|| {
        if let Some(t) = unsafe { checked_ref::<OtelTracer>(tracer) } {
            if !t.vtable.is_null() {
                // SAFETY: `vtable` is live; free the owned tracer context.
                unsafe { ((*t.vtable).tracer_free)(t.ctx) };
            }
        }
        // SAFETY: forwarded to the caller's contract.
        unsafe { destroy(tracer) };
    });
}

// ---------------------------------------------------------------------------
// Span
// ---------------------------------------------------------------------------

/// Run `f` with a validated `&OtelSpan`, dispatching a status. No-op spans (NULL vtable)
/// return `Ok` without calling `f`.
///
/// # Safety
/// `span` must satisfy the handle contract.
unsafe fn dispatch_span<F>(span: *mut OtelSpan, f: F) -> OtelStatus
where
    F: FnOnce(&OtelImplVtable, *mut c_void) -> OtelStatus,
{
    guard_status(|| {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract (single-thread span use).
        match unsafe { checked_ref::<OtelSpan>(span) } {
            Some(s) if s.vtable.is_null() => OtelStatus::Ok,
            // SAFETY: `s.vtable` is a live registered vtable.
            Some(s) => f(unsafe { &*s.vtable }, s.ctx),
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Set a string attribute on a span.
///
/// # Safety
/// `span` must satisfy the handle contract; the string views must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_string_attribute(
    span: *mut OtelSpan,
    key: OtelStringView,
    value: OtelStringView,
) -> OtelStatus {
    unsafe { dispatch_span(span, |vt, ctx| (vt.span_set_string)(ctx, key, value)) }
}

/// Set a boolean attribute (`0` = false, non-zero = true).
///
/// # Safety
/// `span` must satisfy the handle contract; `key` must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_bool_attribute(
    span: *mut OtelSpan,
    key: OtelStringView,
    value: OtelBool,
) -> OtelStatus {
    unsafe { dispatch_span(span, |vt, ctx| (vt.span_set_bool)(ctx, key, value)) }
}

/// Set an i64 attribute.
///
/// # Safety
/// `span` must satisfy the handle contract; `key` must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_int64_attribute(
    span: *mut OtelSpan,
    key: OtelStringView,
    value: i64,
) -> OtelStatus {
    unsafe { dispatch_span(span, |vt, ctx| (vt.span_set_i64)(ctx, key, value)) }
}

/// Set an f64 attribute.
///
/// # Safety
/// `span` must satisfy the handle contract; `key` must be valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_double_attribute(
    span: *mut OtelSpan,
    key: OtelStringView,
    value: f64,
) -> OtelStatus {
    unsafe { dispatch_span(span, |vt, ctx| (vt.span_set_f64)(ctx, key, value)) }
}

/// Set a typed attribute from an [`OtelKeyValue`], dispatching by tag.
///
/// # Safety
/// `span` must satisfy the handle contract; `attribute` must satisfy its contract.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_attribute(
    span: *mut OtelSpan,
    attribute: OtelKeyValue,
) -> OtelStatus {
    unsafe {
        dispatch_span(span, |vt, ctx| {
            // SAFETY: the union member matching the validated tag is active. Union access
            // is permitted here without an inner `unsafe` block because the enclosing
            // function is `unsafe`.
            match OtelAttributeType::from_u32(attribute.value_type) {
                Some(OtelAttributeType::String) => {
                    (vt.span_set_string)(ctx, attribute.key, attribute.value.string_value)
                }
                Some(OtelAttributeType::Bool) => {
                    (vt.span_set_bool)(ctx, attribute.key, attribute.value.bool_value)
                }
                Some(OtelAttributeType::Int64) => {
                    (vt.span_set_i64)(ctx, attribute.key, attribute.value.int64_value)
                }
                Some(OtelAttributeType::Double) => {
                    (vt.span_set_f64)(ctx, attribute.key, attribute.value.double_value)
                }
                None => fail(
                    OtelStatus::InvalidArgument,
                    "attribute value_type is not a valid OtelAttributeType tag",
                ),
            }
        })
    }
}

/// Add a timestamped event with optional attributes.
///
/// # Safety
/// `span` must satisfy the handle contract; `name` valid; `attributes` valid for `count`.
#[no_mangle]
pub unsafe extern "C" fn otel_span_add_event(
    span: *mut OtelSpan,
    name: OtelStringView,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    unsafe {
        dispatch_span(span, |vt, ctx| {
            (vt.span_add_event)(ctx, name, attributes, attribute_count)
        })
    }
}

/// Set the span status. `code` outside [`OtelSpanStatusCode`] is rejected.
///
/// # Safety
/// `span` must satisfy the handle contract; `description` valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_set_status(
    span: *mut OtelSpan,
    code: u32,
    description: OtelStringView,
) -> OtelStatus {
    unsafe {
        dispatch_span(span, |vt, ctx| {
            if OtelSpanStatusCode::from_u32(code).is_none() {
                return fail(
                    OtelStatus::InvalidArgument,
                    "status code is not a valid OtelSpanStatusCode value",
                );
            }
            (vt.span_set_status)(ctx, code, description)
        })
    }
}

/// Rename a span.
///
/// # Safety
/// `span` must satisfy the handle contract; `name` valid.
#[no_mangle]
pub unsafe extern "C" fn otel_span_update_name(
    span: *mut OtelSpan,
    name: OtelStringView,
) -> OtelStatus {
    unsafe { dispatch_span(span, |vt, ctx| (vt.span_update_name)(ctx, name)) }
}

/// End a span (idempotent).
///
/// # Safety
/// `span` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_span_end(span: *mut OtelSpan) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        // SAFETY: forwarded to the caller's contract.
        match unsafe { checked_ref::<OtelSpan>(span) } {
            Some(s) => {
                s.end();
                OtelStatus::Ok
            }
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Destroy a span handle (no-op on NULL). Best-effort ends it, then frees its context.
///
/// # Safety
/// `span` must be NULL or a live span handle, not used or destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_span_destroy(span: *mut OtelSpan) {
    guard_unit(|| {
        // SAFETY: forwarded to the caller's contract.
        if let Some(s) = unsafe { checked_ref::<OtelSpan>(span) } {
            s.end_and_free_ctx();
        }
        // SAFETY: forwarded to the caller's contract.
        unsafe { destroy(span) };
    });
}
