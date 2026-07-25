use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use opentelemetry_c_abi::{
    OtelImplVtable, OtelKeyValue, OtelMetricInstrumentConfig, OtelMetricsVtable, OtelStatus,
    OtelStringView, OtelVtableHeader, OTEL_IMPL_ABI_VERSION,
};
use opentelemetry_c_api::{
    otel_api_meter_provider_new, otel_api_provider_new, otel_api_register_global_meter_provider,
    otel_api_register_global_meter_provider_with_token, otel_api_unregister_global_meter_provider,
    otel_meter_provider_destroy,
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
    abi_version: OTEL_IMPL_ABI_VERSION,
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

static TOKEN_FREES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn token_free(ctx: *mut c_void) {
    TOKEN_FREES.fetch_add(1, Ordering::SeqCst);
    drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
}

const TOKEN_VTABLE: OtelMetricsVtable = OtelMetricsVtable {
    provider_free: token_free,
    ..VALID
};

#[test]
fn rejects_incompatible_metrics_vtables() {
    let dummy = std::ptr::NonNull::<u8>::dangling().as_ptr().cast();
    for invalid in [
        OtelMetricsVtable {
            abi_version: OTEL_IMPL_ABI_VERSION + 1,
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
        abi_version: OTEL_IMPL_ABI_VERSION,
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
        abi_version: OTEL_IMPL_ABI_VERSION,
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
