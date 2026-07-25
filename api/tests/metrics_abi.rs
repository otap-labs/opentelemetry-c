use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use opentelemetry_c_abi::{
    metrics_vtable_compatible, trace_vtable_compatible, OtelImplVtable, OtelKeyValue,
    OtelMetricInstrumentConfig, OtelMetricsVtable, OtelStatus, OtelStringView, OtelVtableHeader,
    OTEL_IMPL_ABI_VERSION, OTEL_IMPL_VTABLE_REQUIRED_SIZE, OTEL_METRICS_IMPL_ABI_VERSION,
    OTEL_METRICS_VTABLE_REQUIRED_SIZE, OTEL_TRACE_IMPL_ABI_VERSION,
};
use opentelemetry_c_api::{
    otel_api_meter_provider_new, otel_api_provider_new, otel_api_register_global_meter_provider,
    otel_api_register_global_meter_provider_with_token, otel_api_register_global_provider,
    otel_api_unregister_global_meter_provider, otel_meter_provider_destroy,
};

extern "C" fn get_meter(
    _: *mut c_void,
    _: OtelStringView,
    _: OtelStringView,
    _: OtelStringView,
) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn retain(ctx: *mut c_void) -> *mut c_void {
    ctx
}

extern "C" fn free(_: *mut c_void) {}

extern "C" fn create(_: *mut c_void, _: *const OtelMetricInstrumentConfig) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn record_u64(_: *mut c_void, _: u64, _: *const OtelKeyValue, _: usize) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn record_i64(_: *mut c_void, _: i64, _: *const OtelKeyValue, _: usize) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn record_f64(_: *mut c_void, _: f64, _: *const OtelKeyValue, _: usize) -> OtelStatus {
    OtelStatus::Ok
}

const VALID: OtelMetricsVtable = OtelMetricsVtable {
    abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelMetricsVtable>(),
    provider_get_meter: get_meter,
    provider_retain: retain,
    provider_free: free,
    meter_create_instrument: create,
    meter_free: free,
    instrument_record_u64: record_u64,
    instrument_record_i64: record_i64,
    instrument_record_f64: record_f64,
    observer_observe_u64: record_u64,
    observer_observe_i64: record_i64,
    observer_observe_f64: record_f64,
    instrument_free: free,
};

extern "C" fn get_tracer(
    _: *mut c_void,
    _: OtelStringView,
    _: OtelStringView,
    _: OtelStringView,
) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn start_span(_: *mut c_void, _: OtelStringView, _: u32, _: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn set_string(_: *mut c_void, _: OtelStringView, _: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn set_bool(_: *mut c_void, _: OtelStringView, _: u32) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn set_i64(_: *mut c_void, _: OtelStringView, _: i64) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn set_f64(_: *mut c_void, _: OtelStringView, _: f64) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn add_event(
    _: *mut c_void,
    _: OtelStringView,
    _: *const OtelKeyValue,
    _: usize,
) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn set_status(_: *mut c_void, _: u32, _: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}

extern "C" fn update_name(_: *mut c_void, _: OtelStringView) -> OtelStatus {
    OtelStatus::Ok
}

const VALID_TRACE: OtelImplVtable = OtelImplVtable {
    abi_version: OTEL_TRACE_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelImplVtable>(),
    provider_get_tracer: get_tracer,
    provider_retain: retain,
    provider_free: free,
    tracer_start_span: start_span,
    tracer_free: free,
    span_set_string: set_string,
    span_set_bool: set_bool,
    span_set_i64: set_i64,
    span_set_f64: set_f64,
    span_add_event: add_event,
    span_set_status: set_status,
    span_update_name: update_name,
    span_end: free,
    span_free: free,
};

static TOKEN_FREES: AtomicUsize = AtomicUsize::new(0);
static REJECTED_CONTEXT_FREES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn token_free(ctx: *mut c_void) {
    TOKEN_FREES.fetch_add(1, Ordering::SeqCst);
    drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
}

const TOKEN_VTABLE: OtelMetricsVtable = OtelMetricsVtable {
    provider_free: token_free,
    ..VALID
};

extern "C" fn rejected_context_free(_: *mut c_void) {
    REJECTED_CONTEXT_FREES.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn vtable_kind_and_size_validation_is_signal_specific() {
    assert_eq!(OTEL_IMPL_ABI_VERSION, OTEL_TRACE_IMPL_ABI_VERSION);
    assert_ne!(OTEL_TRACE_IMPL_ABI_VERSION, OTEL_METRICS_IMPL_ABI_VERSION);
    assert_eq!(OTEL_METRICS_IMPL_ABI_VERSION & 0xFF00_0000, 0x4D00_0000);
    assert_eq!(OTEL_METRICS_IMPL_ABI_VERSION & 0x00FF_FFFF, 1);

    assert!(unsafe { trace_vtable_compatible(&VALID_TRACE) });
    assert!(unsafe { metrics_vtable_compatible(&VALID) });

    // Cross-kind checks read only the common header and reject before any function slot.
    assert!(!unsafe { metrics_vtable_compatible((&VALID_TRACE as *const OtelImplVtable).cast()) });
    assert!(!unsafe { trace_vtable_compatible((&VALID as *const OtelMetricsVtable).cast()) });

    let truncated_trace = OtelImplVtable {
        struct_size: OTEL_IMPL_VTABLE_REQUIRED_SIZE - 1,
        ..VALID_TRACE
    };
    let truncated_metrics = OtelMetricsVtable {
        struct_size: OTEL_METRICS_VTABLE_REQUIRED_SIZE - 1,
        ..VALID
    };
    assert!(!unsafe { trace_vtable_compatible(&truncated_trace) });
    assert!(!unsafe { metrics_vtable_compatible(&truncated_metrics) });

    let extended_trace = OtelImplVtable {
        struct_size: OTEL_IMPL_VTABLE_REQUIRED_SIZE + 64,
        ..VALID_TRACE
    };
    let extended_metrics = OtelMetricsVtable {
        struct_size: OTEL_METRICS_VTABLE_REQUIRED_SIZE + 64,
        ..VALID
    };
    assert!(unsafe { trace_vtable_compatible(&extended_trace) });
    assert!(unsafe { metrics_vtable_compatible(&extended_metrics) });
}

#[test]
fn cross_kind_registration_and_construction_reject_before_dispatch() {
    REJECTED_CONTEXT_FREES.store(0, Ordering::SeqCst);
    let metrics = OtelMetricsVtable {
        provider_free: rejected_context_free,
        ..VALID
    };
    let trace = OtelImplVtable {
        provider_free: rejected_context_free,
        ..VALID_TRACE
    };

    let trace_ctx = Box::into_raw(Box::new(1u8)).cast();
    assert_eq!(
        unsafe {
            otel_api_register_global_provider(
                (&metrics as *const OtelMetricsVtable).cast(),
                trace_ctx,
            )
        },
        OtelStatus::InvalidArgument
    );
    assert_eq!(REJECTED_CONTEXT_FREES.load(Ordering::SeqCst), 0);
    drop(unsafe { Box::from_raw(trace_ctx.cast::<u8>()) });

    let metrics_ctx = Box::into_raw(Box::new(2u8)).cast();
    assert_eq!(
        unsafe {
            otel_api_register_global_meter_provider(
                (&trace as *const OtelImplVtable).cast(),
                metrics_ctx,
            )
        },
        OtelStatus::InvalidConfig
    );
    assert_eq!(REJECTED_CONTEXT_FREES.load(Ordering::SeqCst), 0);
    drop(unsafe { Box::from_raw(metrics_ctx.cast::<u8>()) });

    let metrics_provider_ctx = Box::into_raw(Box::new(3u8)).cast();
    assert!(unsafe {
        otel_api_meter_provider_new(
            (&trace as *const OtelImplVtable).cast(),
            metrics_provider_ctx,
        )
    }
    .is_null());
    assert_eq!(REJECTED_CONTEXT_FREES.load(Ordering::SeqCst), 0);
    drop(unsafe { Box::from_raw(metrics_provider_ctx.cast::<u8>()) });

    let trace_provider_ctx = Box::into_raw(Box::new(4u8)).cast();
    assert!(unsafe {
        otel_api_provider_new(
            (&metrics as *const OtelMetricsVtable).cast(),
            trace_provider_ctx,
        )
    }
    .is_null());
    assert_eq!(REJECTED_CONTEXT_FREES.load(Ordering::SeqCst), 0);
    drop(unsafe { Box::from_raw(trace_provider_ctx.cast::<u8>()) });
}

#[test]
fn rejects_incompatible_metrics_vtables() {
    let dummy = std::ptr::NonNull::<u8>::dangling().as_ptr().cast();
    for invalid in [
        OtelMetricsVtable {
            abi_version: OTEL_METRICS_IMPL_ABI_VERSION + 1,
            ..VALID
        },
        OtelMetricsVtable {
            struct_size: std::mem::size_of::<OtelMetricsVtable>() - 1,
            ..VALID
        },
    ] {
        assert_eq!(
            unsafe { otel_api_register_global_meter_provider(&invalid, dummy) },
            OtelStatus::InvalidConfig
        );
        let provider = unsafe { otel_api_meter_provider_new(&invalid, dummy) };
        assert!(provider.is_null());
    }

    let provider = unsafe { otel_api_meter_provider_new(&VALID, dummy) };
    assert!(!provider.is_null());
    unsafe { otel_meter_provider_destroy(provider) };
}

#[test]
fn rejects_truncated_vtable_prefixes_without_full_struct_access() {
    let metrics_header = OtelVtableHeader {
        abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
        struct_size: std::mem::size_of::<OtelVtableHeader>(),
    };
    let metrics = (&metrics_header as *const OtelVtableHeader).cast::<OtelMetricsVtable>();
    let dummy = std::ptr::NonNull::<u8>::dangling().as_ptr().cast();
    assert_eq!(
        unsafe { otel_api_register_global_meter_provider(metrics, dummy) },
        OtelStatus::InvalidConfig
    );
    assert!(unsafe { otel_api_meter_provider_new(metrics, dummy) }.is_null());

    let trace_header = OtelVtableHeader {
        abi_version: OTEL_TRACE_IMPL_ABI_VERSION,
        struct_size: std::mem::size_of::<OtelVtableHeader>(),
    };
    let trace = (&trace_header as *const OtelVtableHeader).cast::<OtelImplVtable>();
    assert!(unsafe { otel_api_provider_new(trace, dummy) }.is_null());
}

#[test]
fn metrics_registration_tokens_only_clear_the_current_provider() {
    TOKEN_FREES.store(0, Ordering::SeqCst);
    let mut first_id = 0;
    let mut second_id = 0;
    assert_eq!(
        unsafe {
            otel_api_register_global_meter_provider_with_token(
                &TOKEN_VTABLE,
                Box::into_raw(Box::new(1u8)).cast(),
                &mut first_id,
            )
        },
        OtelStatus::Ok
    );
    assert_eq!(
        unsafe {
            otel_api_register_global_meter_provider_with_token(
                &TOKEN_VTABLE,
                Box::into_raw(Box::new(2u8)).cast(),
                &mut second_id,
            )
        },
        OtelStatus::Ok
    );
    assert_ne!(first_id, 0);
    assert_ne!(second_id, 0);
    assert_ne!(first_id, second_id);
    assert_eq!(TOKEN_FREES.load(Ordering::SeqCst), 1);

    assert_eq!(
        otel_api_unregister_global_meter_provider(first_id),
        OtelStatus::Ok
    );
    assert_eq!(TOKEN_FREES.load(Ordering::SeqCst), 1);
    assert_eq!(
        otel_api_unregister_global_meter_provider(second_id),
        OtelStatus::Ok
    );
    assert_eq!(TOKEN_FREES.load(Ordering::SeqCst), 2);
}
