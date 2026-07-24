//! API-owned process-global Metrics provider slot.

use std::os::raw::c_void;
use std::sync::RwLock;

use opentelemetry_c_abi::{metrics_vtable_compatible, OtelMetricsVtable};

use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::{guard_ptr, guard_status, into_raw};
use crate::metrics::{MeterProviderInner, OtelMeterProvider};

struct GlobalMeterProvider {
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
}

// SAFETY: contexts are SDK-owned, reference-counted providers accessed only through a
// concurrency-safe vtable.
unsafe impl Send for GlobalMeterProvider {}
unsafe impl Sync for GlobalMeterProvider {}

static GLOBAL_METRICS: RwLock<GlobalMeterProvider> = RwLock::new(GlobalMeterProvider {
    vtable: std::ptr::null(),
    ctx: std::ptr::null_mut(),
});

pub(crate) enum GlobalMetricsRetain {
    NoProvider,
    Retained {
        vtable: *const OtelMetricsVtable,
        ctx: *mut c_void,
    },
    RetainFailed,
}

pub(crate) fn retain_global_metrics() -> GlobalMetricsRetain {
    let global = GLOBAL_METRICS.read().unwrap_or_else(|p| p.into_inner());
    if global.vtable.is_null() {
        return GlobalMetricsRetain::NoProvider;
    }
    // SAFETY: replacement requires the write lock, so the slot context remains alive.
    let retained = unsafe { ((*global.vtable).provider_retain)(global.ctx) };
    if retained.is_null() {
        if !has_last_error() {
            set_last_error("global meter provider retain failed");
        }
        return GlobalMetricsRetain::RetainFailed;
    }
    GlobalMetricsRetain::Retained {
        vtable: global.vtable,
        ctx: retained,
    }
}

/// Internal SDK registration entry point for the Metrics provider.
///
/// # Safety
///
/// `vtable` must point to a live static-compatible vtable and `provider_ctx` must be one
/// owned provider reference accepted by that vtable.
#[no_mangle]
pub unsafe extern "C" fn otel_api_register_global_meter_provider(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if vtable.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "register_global_meter_provider: vtable must not be NULL",
            );
        }
        // SAFETY: non-NULL and required by the registration contract.
        if !metrics_vtable_compatible(unsafe { &*vtable }) {
            return fail(
                OtelStatus::InvalidConfig,
                "register_global_meter_provider: incompatible metrics implementation ABI",
            );
        }
        let old = {
            let mut global = GLOBAL_METRICS.write().unwrap_or_else(|p| p.into_inner());
            let old = GlobalMeterProvider {
                vtable: global.vtable,
                ctx: global.ctx,
            };
            global.vtable = vtable;
            global.ctx = provider_ctx;
            old
        };
        if !old.vtable.is_null() {
            unsafe { ((*old.vtable).provider_free)(old.ctx) };
        }
        OtelStatus::Ok
    })
}

/// Internal SDK entry point that wraps an owned Metrics provider context in an API handle.
///
/// # Safety
///
/// `vtable` must remain live for the returned handle's lifetime and `provider_ctx` must be
/// one owned provider reference accepted by that vtable.
#[no_mangle]
pub unsafe extern "C" fn otel_api_meter_provider_new(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
) -> *mut OtelMeterProvider {
    guard_ptr(|| {
        clear_last_error();
        if vtable.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "meter_provider_new: vtable must not be NULL",
            );
            return std::ptr::null_mut();
        }
        if !metrics_vtable_compatible(unsafe { &*vtable }) {
            fail(
                OtelStatus::InvalidConfig,
                "meter_provider_new: incompatible metrics implementation ABI",
            );
            return std::ptr::null_mut();
        }
        into_raw(OtelMeterProvider::new(MeterProviderInner::Backed {
            vtable,
            ctx: provider_ctx,
        }))
    })
}

/// Return an owned lazy handle to the API-owned global MeterProvider.
#[no_mangle]
pub extern "C" fn otel_global_meter_provider() -> *mut OtelMeterProvider {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelMeterProvider::new(MeterProviderInner::Global))
    })
}
