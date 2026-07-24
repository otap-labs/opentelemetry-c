use std::ffi::c_void;

use opentelemetry_c_abi::{
    OtelKeyValue, OtelMetricInstrumentConfig, OtelMetricsVtable, OtelStatus, OtelStringView,
    OTEL_IMPL_ABI_VERSION,
};
use opentelemetry_c_api::{
    otel_api_meter_provider_new, otel_api_register_global_meter_provider,
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
