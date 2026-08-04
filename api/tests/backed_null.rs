//! Backed-implementation failure propagation (contract for item 2 of the hardening spec).
//!
//! A *backed* provider/tracer (one with a real vtable) that fails — e.g. because a caller
//! passed a malformed `otel_string_view_t` — must surface as **NULL** with the last-error
//! left set, NOT as a success-shaped no-op handle. The genuinely unbacked path (no SDK
//! installed) must still return valid no-op handles.
//!
//! The test installs a minimal vtable whose `provider_get_tracer` / `tracer_start_span`
//! reproduce the SDK's behavior: convert the name string view and, on the abi's
//! validation error, set the last-error and return NULL. This exercises the API's
//! propagation logic directly without depending on the SDK crate.

use std::os::raw::{c_char, c_void};

use opentelemetry_c_abi::{
    OtelImplVtable, OtelKeyValue, OtelSpanContextView, OtelStatus, OtelStringView,
    OTEL_IMPL_VTABLE_REQUIRED_SIZE, OTEL_IMPL_VTABLE_SPAN_CONTEXT_SIZE,
    OTEL_IMPL_VTABLE_SPAN_START_EX_SIZE,
};

use opentelemetry_c_api::{
    otel_api_provider_new, otel_api_set_last_error, otel_global_tracer_provider,
    otel_last_error_message, otel_span_context_destroy, otel_span_destroy, otel_span_end,
    otel_span_get_context, otel_tracer_destroy, otel_tracer_provider_destroy,
    otel_tracer_provider_get_tracer, otel_tracer_start_span, otel_tracer_start_span_ex,
    otel_tracer_start_span_with_context, otel_tracer_supports_context, OtelSpan, OtelSpanContext,
    OtelSpanStartOptions, OtelSpanStartOptionsEx, OTEL_PARENT_AMBIENT,
};

// ---- A minimal backed vtable that validates the name like the real SDK ----

fn set_err(msg: &str) {
    // SAFETY: `msg` is a valid UTF-8 byte range for the duration of the call.
    unsafe { otel_api_set_last_error(msg.as_ptr().cast::<c_char>(), msg.len()) };
}

/// Validate `name` exactly as the SDK does; on a malformed view, set the last-error and
/// return `false` (the caller then returns NULL).
fn name_is_valid(name: OtelStringView) -> bool {
    // SAFETY: forwarded to the abi contract; returns Err on NULL+len or oversized len.
    match unsafe { name.to_string_lossy() } {
        Ok(_) => true,
        Err(_) => {
            set_err("malformed name string view");
            false
        }
    }
}

fn dummy() -> *mut c_void {
    Box::into_raw(Box::new(0u8)) as *mut c_void
}
unsafe fn free_dummy(ctx: *mut c_void) {
    if !ctx.is_null() {
        drop(unsafe { Box::from_raw(ctx as *mut u8) });
    }
}

extern "C" fn vt_provider_get_tracer(
    _c: *mut c_void,
    name: OtelStringView,
    _v: OtelStringView,
    _s: OtelStringView,
) -> *mut c_void {
    if !name_is_valid(name) {
        return std::ptr::null_mut();
    }
    dummy()
}
extern "C" fn vt_provider_retain(_c: *mut c_void) -> *mut c_void {
    dummy()
}
extern "C" fn vt_provider_free(c: *mut c_void) {
    unsafe { free_dummy(c) };
}
extern "C" fn vt_tracer_start_span(
    _c: *mut c_void,
    name: OtelStringView,
    _k: u32,
    _p: *mut c_void,
) -> *mut c_void {
    if !name_is_valid(name) {
        return std::ptr::null_mut();
    }
    dummy()
}
extern "C" fn vt_tracer_free(c: *mut c_void) {
    unsafe { free_dummy(c) };
}
extern "C" fn vt_span_str(_c: *mut c_void, _k: OtelStringView, _v: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_bool(_c: *mut c_void, _k: OtelStringView, _v: u32) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_i64(_c: *mut c_void, _k: OtelStringView, _v: i64) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_f64(_c: *mut c_void, _k: OtelStringView, _v: f64) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_event(
    _c: *mut c_void,
    _n: OtelStringView,
    _a: *const OtelKeyValue,
    _cnt: usize,
) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_status(_c: *mut c_void, _code: u32, _d: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_update(_c: *mut c_void, _n: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}
extern "C" fn vt_span_end(_c: *mut c_void) {}
extern "C" fn vt_span_free(c: *mut c_void) {
    unsafe { free_dummy(c) };
}
extern "C" fn vt_span_context_visit(
    _c: *mut c_void,
    visitor: opentelemetry_c_abi::OtelSpanContextVisitor,
    user_data: *mut c_void,
) -> OtelStatus {
    let Some(visitor) = visitor else {
        return OtelStatus::InvalidArgument;
    };
    let context = OtelSpanContextView {
        trace_id: [1; 16],
        span_id: [2; 8],
        trace_flags: 1,
        reserved: [0; 3],
        is_remote: 0,
        trace_state: OtelStringView::empty(),
    };
    visitor(user_data, &context)
}
extern "C" fn vt_start_with_context(
    _c: *mut c_void,
    _n: OtelStringView,
    _k: u32,
    _p: *const opentelemetry_c_abi::OtelSpanContextView,
) -> *mut c_void {
    std::ptr::null_mut()
}
extern "C" fn vt_start_span_ex(
    _c: *mut c_void,
    name: OtelStringView,
    _cfg: *const opentelemetry_c_abi::OtelSpanStartConfig,
) -> *mut c_void {
    if !name_is_valid(name) {
        return std::ptr::null_mut();
    }
    dummy()
}
extern "C" fn vt_start_span_ex_with_context(
    ctx: *mut c_void,
    name: OtelStringView,
    config: *const opentelemetry_c_abi::OtelSpanStartConfig,
    _: *const opentelemetry_c_abi::OtelContextView,
) -> *mut c_void {
    vt_start_span_ex(ctx, name, config)
}
extern "C" fn vt_span_context_skip(
    _c: *mut c_void,
    _visitor: opentelemetry_c_abi::OtelSpanContextVisitor,
    _user_data: *mut c_void,
) -> OtelStatus {
    OtelStatus::Ok
}

static BACKED_VTABLE: OtelImplVtable = OtelImplVtable {
    abi_version: opentelemetry_c_abi::OTEL_TRACE_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelImplVtable>(),
    provider_get_tracer: vt_provider_get_tracer,
    provider_retain: vt_provider_retain,
    provider_free: vt_provider_free,
    tracer_start_span: vt_tracer_start_span,
    tracer_free: vt_tracer_free,
    span_set_string: vt_span_str,
    span_set_bool: vt_span_bool,
    span_set_i64: vt_span_i64,
    span_set_f64: vt_span_f64,
    span_add_event: vt_span_event,
    span_set_status: vt_span_status,
    span_update_name: vt_span_update,
    span_end: vt_span_end,
    span_free: vt_span_free,
    span_context_visit: vt_span_context_visit,
    tracer_start_span_with_context: vt_start_with_context,
    tracer_start_span_ex: vt_start_span_ex,
    tracer_start_span_ex_with_context: vt_start_span_ex_with_context,
};

fn good(s: &'static str) -> OtelStringView {
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
/// A malformed view: NULL pointer with a non-zero length (rejected by the abi).
fn malformed() -> OtelStringView {
    OtelStringView {
        ptr: std::ptr::null(),
        len: 5,
    }
}
fn last_error_is_set() -> bool {
    !otel_last_error_message().ptr.is_null()
}

fn last_error() -> String {
    let error = otel_last_error_message();
    assert!(!error.ptr.is_null());
    String::from_utf8(
        unsafe { std::slice::from_raw_parts(error.ptr.cast::<u8>(), error.len) }.to_vec(),
    )
    .unwrap()
}

fn backed_provider() -> *mut opentelemetry_c_api::OtelTracerProvider {
    // SAFETY: BACKED_VTABLE is 'static; the ctx is an owned dummy Box freed on destroy.
    unsafe { otel_api_provider_new(&BACKED_VTABLE, dummy()) }
}

#[test]
fn backed_provider_get_tracer_malformed_name_returns_null() {
    let provider = backed_provider();
    assert!(!provider.is_null());

    // A malformed name view: the backed vtable fails, so the API must return NULL (not a
    // no-op tracer) and leave the last-error set.
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, malformed(), empty(), empty()) };
    assert!(
        tracer.is_null(),
        "backed get_tracer failure must return NULL"
    );
    assert!(
        last_error_is_set(),
        "last-error must remain set after failure"
    );

    // A well-formed name succeeds (proves the vtable is otherwise functional).
    let ok = unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    assert!(
        !ok.is_null(),
        "backed get_tracer with a valid name must succeed"
    );
    unsafe { otel_tracer_destroy(ok) };

    unsafe { otel_tracer_provider_destroy(provider) };
}

#[test]
fn backed_tracer_start_span_malformed_name_returns_null() {
    let provider = backed_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    assert!(!tracer.is_null());

    // A malformed span name: the backed tracer fails, so the API must return NULL.
    let span: *mut OtelSpan =
        unsafe { otel_tracer_start_span(tracer, malformed(), std::ptr::null()) };
    assert!(span.is_null(), "backed start_span failure must return NULL");
    assert!(
        last_error_is_set(),
        "last-error must remain set after failure"
    );

    // A well-formed name succeeds.
    let ok: *mut OtelSpan = unsafe { otel_tracer_start_span(tracer, good("op"), std::ptr::null()) };
    assert!(
        !ok.is_null(),
        "backed start_span with a valid name must succeed"
    );
    unsafe {
        otel_span_end(ok);
        otel_span_destroy(ok);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn span_context_apis_fail_closed_with_original_vtable_prefix() {
    let provider = backed_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    let span = unsafe { otel_tracer_start_span(tracer, good("parent"), std::ptr::null()) };
    let mut context: *mut OtelSpanContext = std::ptr::null_mut();
    assert_eq!(
        unsafe { otel_span_get_context(span, &mut context) },
        OtelStatus::Ok
    );
    assert!(!context.is_null());

    let legacy_vtable = OtelImplVtable {
        struct_size: OTEL_IMPL_VTABLE_REQUIRED_SIZE,
        ..BACKED_VTABLE
    };
    let legacy_provider = unsafe { otel_api_provider_new(&legacy_vtable, dummy()) };
    let legacy_tracer = unsafe {
        otel_tracer_provider_get_tracer(legacy_provider, good("legacy"), empty(), empty())
    };
    let legacy_span =
        unsafe { otel_tracer_start_span(legacy_tracer, good("legacy-span"), std::ptr::null()) };

    let mut unavailable: *mut OtelSpanContext = std::ptr::null_mut();
    assert_eq!(
        unsafe { otel_span_get_context(legacy_span, &mut unavailable) },
        OtelStatus::InvalidConfig
    );
    assert!(unavailable.is_null());
    assert!(last_error().contains("does not support span-context snapshots"));

    assert!(unsafe {
        otel_tracer_start_span_with_context(legacy_tracer, good("child"), std::ptr::null(), context)
    }
    .is_null());
    assert!(last_error().contains("does not support context parenting"));

    unsafe {
        otel_span_destroy(legacy_span);
        otel_tracer_destroy(legacy_tracer);
        otel_tracer_provider_destroy(legacy_provider);
        otel_span_context_destroy(context);
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn span_context_snapshot_requires_the_sdk_to_invoke_the_visitor() {
    let vtable = OtelImplVtable {
        span_context_visit: vt_span_context_skip,
        ..BACKED_VTABLE
    };
    let provider = unsafe { otel_api_provider_new(&vtable, dummy()) };
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    let span = unsafe { otel_tracer_start_span(tracer, good("parent"), std::ptr::null()) };
    let mut context: *mut OtelSpanContext = std::ptr::null_mut();
    assert_eq!(
        unsafe { otel_span_get_context(span, &mut context) },
        OtelStatus::InternalError
    );
    assert!(context.is_null());
    assert!(last_error().contains("did not return a span context"));

    unsafe {
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn no_sdk_path_returns_noop_handles() {
    // This test file never installs a global SDK, so the global slot is empty: the global
    // provider must yield valid *no-op* handles, and the no-op path must not set an error.
    let provider = otel_global_tracer_provider();
    assert!(!provider.is_null());

    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    assert!(
        !tracer.is_null(),
        "unbacked get_tracer must return a no-op tracer"
    );
    assert!(!last_error_is_set(), "the no-op path must not set an error");

    let span: *mut OtelSpan =
        unsafe { otel_tracer_start_span(tracer, good("op"), std::ptr::null()) };
    assert!(
        !span.is_null(),
        "unbacked start_span must return a no-op span"
    );

    let opts = OtelSpanStartOptions {
        kind: 0,
        parent: span,
    };
    let child: *mut OtelSpan = unsafe { otel_tracer_start_span(tracer, good("child"), &opts) };
    assert!(!child.is_null(), "no-op child span must be valid");

    unsafe {
        otel_span_end(child);
        otel_span_destroy(child);
        otel_span_end(span);
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

fn opts_ex() -> OtelSpanStartOptionsEx {
    OtelSpanStartOptionsEx {
        struct_size: std::mem::size_of::<OtelSpanStartOptionsEx>(),
        kind: 0,
        reserved: 0,
        parent: std::ptr::null(),
        parent_context: std::ptr::null(),
        start_time_unix_nanos: 0,
        attributes: std::ptr::null(),
        attribute_count: 0,
        links: std::ptr::null(),
        link_count: 0,
        parent_mode: 0,
        reserved2: 0,
    }
}

#[test]
fn start_span_ex_noop_tracer_returns_noop_span() {
    // An unbacked (no-SDK) tracer must yield a valid no-op span and set no error.
    let provider = otel_global_tracer_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    let opts = opts_ex();
    let span: *mut OtelSpan = unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &opts) };
    assert!(!span.is_null(), "no-op start_span_ex must return a span");
    assert!(!last_error_is_set());
    unsafe {
        otel_span_end(span);
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

// Exactly the required prefix of `OtelSpanStartOptionsEx` (through `start_time_unix_nanos`),
// as a genuinely older caller would compile it. The backing allocation is only this many
// bytes, so any read past `struct_size` is a real out-of-bounds access (caught by Miri/ASan),
// not merely a logical truncation of a full struct.
#[repr(C)]
struct SpanStartOptionsExPrefix {
    struct_size: usize,
    kind: u32,
    reserved: u32,
    parent: *const OtelSpan,
    parent_context: *const OtelSpanContext,
    start_time_unix_nanos: u64,
}

#[test]
fn start_span_ex_accepts_truncated_backing_storage() {
    assert_eq!(
        std::mem::size_of::<SpanStartOptionsExPrefix>(),
        std::mem::offset_of!(OtelSpanStartOptionsEx, attributes),
        "prefix struct must match the required-size boundary exactly"
    );
    let make_prefix = || SpanStartOptionsExPrefix {
        struct_size: std::mem::size_of::<SpanStartOptionsExPrefix>(),
        kind: 0,
        reserved: 0,
        parent: std::ptr::null(),
        parent_context: std::ptr::null(),
        start_time_unix_nanos: 0,
    };

    // No-op tracer: the prefix fields are read before the NULL-vtable check, so this alone
    // exercises the truncated read path.
    let provider = otel_global_tracer_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    let prefix = make_prefix();
    let span = unsafe {
        otel_tracer_start_span_ex(
            tracer,
            good("ex"),
            (&prefix as *const SpanStartOptionsExPrefix).cast(),
        )
    };
    assert!(
        !span.is_null(),
        "no-op prefix start_span_ex must return a span"
    );
    assert!(!last_error_is_set());
    unsafe {
        otel_span_end(span);
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }

    // Backed tracer: the same prefix drives the extended vtable entry to a real span.
    let provider = backed_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };
    let prefix = make_prefix();
    let span = unsafe {
        otel_tracer_start_span_ex(
            tracer,
            good("ex"),
            (&prefix as *const SpanStartOptionsExPrefix).cast(),
        )
    };
    assert!(!span.is_null(), "backed prefix start_span_ex must succeed");
    unsafe {
        otel_span_end(span);
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn start_span_ex_backed_success_and_validation() {
    let provider = backed_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("instr"), empty(), empty()) };

    // Success on a backed tracer that supports the extended entry.
    let opts = opts_ex();
    let span = unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &opts) };
    assert!(!span.is_null(), "backed start_span_ex must succeed");
    unsafe {
        otel_span_end(span);
        otel_span_destroy(span);
    }

    // NULL options are rejected.
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ex"), std::ptr::null()) }.is_null());

    // struct_size below the required minimum is rejected.
    let mut small = opts_ex();
    small.struct_size = 8;
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &small) }.is_null());

    // Non-zero reserved is rejected.
    let mut bad_reserved = opts_ex();
    bad_reserved.reserved = 1;
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &bad_reserved) }.is_null());

    // A live parent and a context parent are mutually exclusive.
    let live = unsafe { otel_tracer_start_span(tracer, good("p"), std::ptr::null()) };
    let mut ctx: *mut OtelSpanContext = std::ptr::null_mut();
    let _ = unsafe { otel_span_get_context(live, &mut ctx) };
    let mut both = opts_ex();
    both.parent = live;
    both.parent_context = ctx;
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &both) }.is_null());

    unsafe {
        otel_span_context_destroy(ctx);
        otel_span_end(live);
        otel_span_destroy(live);
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn start_span_ex_requires_ex_support() {
    // A backed vtable whose struct_size predates the extended entry must fail closed.
    let legacy_vtable = OtelImplVtable {
        struct_size: OTEL_IMPL_VTABLE_SPAN_CONTEXT_SIZE,
        ..BACKED_VTABLE
    };
    let provider = unsafe { otel_api_provider_new(&legacy_vtable, dummy()) };
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("legacy"), empty(), empty()) };
    let opts = opts_ex();
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ex"), &opts) }.is_null());
    assert!(last_error().contains("does not support extended span start"));
    unsafe {
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}

#[test]
fn ambient_context_fails_closed_with_pre_context_vtable() {
    let old_vtable = OtelImplVtable {
        struct_size: OTEL_IMPL_VTABLE_SPAN_START_EX_SIZE,
        ..BACKED_VTABLE
    };
    let provider = unsafe { otel_api_provider_new(&old_vtable, dummy()) };
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, good("old"), empty(), empty()) };
    assert_eq!(unsafe { otel_tracer_supports_context(tracer) }, 0);
    let mut options = opts_ex();
    options.parent_mode = OTEL_PARENT_AMBIENT;
    assert!(unsafe { otel_tracer_start_span_ex(tracer, good("ambient"), &options) }.is_null());
    assert!(last_error().contains("does not support ambient context"));
    unsafe {
        otel_tracer_destroy(tracer);
        otel_tracer_provider_destroy(provider);
    }
}
