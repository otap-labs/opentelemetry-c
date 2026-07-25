//! API-owned process-global Metrics provider slot.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use opentelemetry_c_abi::{metrics_vtable_compatible, OtelMetricsVtable};

use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::{guard_ptr, guard_status, into_raw};
use crate::metrics::{MeterProviderInner, OtelMeterProvider};

struct GlobalMeterProvider {
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
    registration_id: u64,
}

// SAFETY: contexts are SDK-owned, reference-counted providers accessed only through a
// concurrency-safe vtable.
unsafe impl Send for GlobalMeterProvider {}
unsafe impl Sync for GlobalMeterProvider {}

static GLOBAL_METRICS: RwLock<GlobalMeterProvider> = RwLock::new(GlobalMeterProvider {
    vtable: std::ptr::null(),
    ctx: std::ptr::null_mut(),
    registration_id: 0,
});
static NEXT_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

fn next_registration_id() -> u64 {
    loop {
        let id = NEXT_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

fn install_global_meter_provider(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
) -> Result<u64, OtelStatus> {
    if vtable.is_null() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "register_global_meter_provider: vtable must not be NULL",
        ));
    }
    if !unsafe { metrics_vtable_compatible(vtable) } {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "register_global_meter_provider: incompatible metrics implementation ABI",
        ));
    }
    let registration_id = next_registration_id();
    let old = {
        let mut global = GLOBAL_METRICS.write().unwrap_or_else(|p| p.into_inner());
        let old = GlobalMeterProvider {
            vtable: global.vtable,
            ctx: global.ctx,
            registration_id: global.registration_id,
        };
        global.vtable = vtable;
        global.ctx = provider_ctx;
        global.registration_id = registration_id;
        old
    };
    if !old.vtable.is_null() {
        unsafe { ((*old.vtable).provider_free)(old.ctx) };
    }
    Ok(registration_id)
}

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
        match install_global_meter_provider(vtable, provider_ctx) {
            Ok(_) => OtelStatus::Ok,
            Err(status) => status,
        }
    })
}

/// Install the Metrics provider and return a token for conditional deregistration.
///
/// # Safety
///
/// `vtable` and `provider_ctx` follow [`otel_api_register_global_meter_provider`]. `out_id`
/// must point to writable storage. On success, ownership of `provider_ctx` transfers to the
/// global slot and `out_id` receives a non-zero registration token.
#[no_mangle]
pub unsafe extern "C" fn otel_api_register_global_meter_provider_with_token(
    vtable: *const OtelMetricsVtable,
    provider_ctx: *mut c_void,
    out_id: *mut u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out_id.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "register_global_meter_provider_with_token: out_id must not be NULL",
            );
        }
        unsafe { *out_id = 0 };
        match install_global_meter_provider(vtable, provider_ctx) {
            Ok(id) => {
                unsafe { *out_id = id };
                OtelStatus::Ok
            }
            Err(status) => status,
        }
    })
}

/// Clear the global Metrics provider only if `registration_id` still owns the slot.
///
/// A stale token is a successful no-op, preventing an older SDK from clearing a provider
/// installed later by another SDK.
#[no_mangle]
pub extern "C" fn otel_api_unregister_global_meter_provider(registration_id: u64) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if registration_id == 0 {
            return fail(
                OtelStatus::InvalidArgument,
                "unregister_global_meter_provider: registration_id must not be zero",
            );
        }
        let old = {
            let mut global = GLOBAL_METRICS.write().unwrap_or_else(|p| p.into_inner());
            if global.registration_id != registration_id {
                return OtelStatus::Ok;
            }
            let old = GlobalMeterProvider {
                vtable: global.vtable,
                ctx: global.ctx,
                registration_id: global.registration_id,
            };
            global.vtable = std::ptr::null();
            global.ctx = std::ptr::null_mut();
            global.registration_id = 0;
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
        if !unsafe { metrics_vtable_compatible(vtable) } {
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
