//! API-owned process-global Logs provider slot.
//!
//! Mirrors the corrected Metrics lifecycle (token-based registration, stale-token-safe
//! unregistration, retain under the read lock, provider release outside the write lock)
//! rather than the older Trace behavior.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use opentelemetry_c_abi::{logs_vtable_compatible, OtelLogsVtable};

use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::{guard_ptr, guard_status, into_raw};
use crate::logs::{LoggerProviderInner, OtelLoggerProvider};

struct GlobalLoggerProvider {
    vtable: *const OtelLogsVtable,
    ctx: *mut c_void,
    registration_id: u64,
}

// SAFETY: contexts are SDK-owned, reference-counted providers accessed only through a
// concurrency-safe vtable.
unsafe impl Send for GlobalLoggerProvider {}
unsafe impl Sync for GlobalLoggerProvider {}

static GLOBAL_LOGS: RwLock<GlobalLoggerProvider> = RwLock::new(GlobalLoggerProvider {
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

/// Install `vtable`/`provider_ctx`, returning the new registration token.
///
/// The previous provider is released **outside** the write lock so a slow SDK-side
/// destructor cannot block concurrent readers, and so provider release never runs while the
/// global lock is held.
fn install_global_logger_provider(
    vtable: *const OtelLogsVtable,
    provider_ctx: *mut c_void,
) -> Result<u64, OtelStatus> {
    if vtable.is_null() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "register_global_logger_provider: vtable must not be NULL",
        ));
    }
    // SAFETY: the caller guarantees the pointer is readable for `OtelVtableHeader`. Only the
    // header is inspected here, before any function slot could be reached.
    if !unsafe { logs_vtable_compatible(vtable) } {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "register_global_logger_provider: incompatible logs implementation ABI",
        ));
    }
    let registration_id = next_registration_id();
    let old = {
        let mut global = GLOBAL_LOGS.write().unwrap_or_else(|p| p.into_inner());
        let old = GlobalLoggerProvider {
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
        // SAFETY: the slot held exactly one owned reference to this context.
        unsafe { ((*old.vtable).provider_free)(old.ctx) };
    }
    Ok(registration_id)
}

pub(crate) enum GlobalLogsRetain {
    NoProvider,
    Retained {
        vtable: *const OtelLogsVtable,
        ctx: *mut c_void,
    },
    RetainFailed,
}

/// Take an independent owned reference to the installed provider, if any.
pub(crate) fn retain_global_logs() -> GlobalLogsRetain {
    let global = GLOBAL_LOGS.read().unwrap_or_else(|p| p.into_inner());
    if global.vtable.is_null() {
        return GlobalLogsRetain::NoProvider;
    }
    // SAFETY: replacement requires the write lock, so the slot context stays alive here.
    let retained = unsafe { ((*global.vtable).provider_retain)(global.ctx) };
    if retained.is_null() {
        if !has_last_error() {
            set_last_error("global logger provider retain failed");
        }
        return GlobalLogsRetain::RetainFailed;
    }
    GlobalLogsRetain::Retained {
        vtable: global.vtable,
        ctx: retained,
    }
}

/// Install the Logs provider and return a token for conditional deregistration.
///
/// # Safety
///
/// `vtable` must be correctly aligned and readable for `OtelVtableHeader`. If its Logs
/// kind/version and size are compatible, it must point to a complete `'static`
/// [`OtelLogsVtable`]. `provider_ctx` must be caller-owned and accepted by that vtable.
/// `out_id` must point to writable storage. Ownership of `provider_ctx` transfers to the
/// global slot **only** on success; on failure it remains caller-owned.
#[no_mangle]
pub unsafe extern "C" fn otel_api_register_global_logger_provider_with_token(
    vtable: *const OtelLogsVtable,
    provider_ctx: *mut c_void,
    out_id: *mut u64,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out_id.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "register_global_logger_provider_with_token: out_id must not be NULL",
            );
        }
        // SAFETY: caller-guaranteed writable storage.
        unsafe { *out_id = 0 };
        match install_global_logger_provider(vtable, provider_ctx) {
            Ok(id) => {
                // SAFETY: as above.
                unsafe { *out_id = id };
                OtelStatus::Ok
            }
            Err(status) => status,
        }
    })
}

/// Clear the global Logs provider only if `registration_id` still owns the slot.
///
/// A stale token is a successful no-op, so an older SDK shutting down cannot clear a
/// provider installed later by a newer SDK.
#[no_mangle]
pub extern "C" fn otel_api_unregister_global_logger_provider(registration_id: u64) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if registration_id == 0 {
            return fail(
                OtelStatus::InvalidArgument,
                "unregister_global_logger_provider: registration_id must not be zero",
            );
        }
        let old = {
            let mut global = GLOBAL_LOGS.write().unwrap_or_else(|p| p.into_inner());
            if global.registration_id != registration_id {
                return OtelStatus::Ok;
            }
            let old = GlobalLoggerProvider {
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
            // SAFETY: the slot held exactly one owned reference to this context.
            unsafe { ((*old.vtable).provider_free)(old.ctx) };
        }
        OtelStatus::Ok
    })
}

/// Internal SDK entry point that wraps an owned Logs provider context in an API handle.
///
/// # Safety
///
/// `vtable` must be correctly aligned and readable for `OtelVtableHeader`. If its Logs
/// kind/version and size are compatible, it must point to a complete [`OtelLogsVtable`]
/// that remains live for the returned handle's lifetime. `provider_ctx` must be caller-owned
/// and accepted by that vtable; its ownership transfers only when a non-NULL handle is
/// returned.
#[no_mangle]
pub unsafe extern "C" fn otel_api_logger_provider_new(
    vtable: *const OtelLogsVtable,
    provider_ctx: *mut c_void,
) -> *mut OtelLoggerProvider {
    guard_ptr(|| {
        clear_last_error();
        if vtable.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "logger_provider_new: vtable must not be NULL",
            );
            return std::ptr::null_mut();
        }
        // SAFETY: caller guarantees header readability; only the header is inspected.
        if !unsafe { logs_vtable_compatible(vtable) } {
            fail(
                OtelStatus::InvalidConfig,
                "logger_provider_new: incompatible logs implementation ABI",
            );
            return std::ptr::null_mut();
        }
        into_raw(OtelLoggerProvider::new(LoggerProviderInner::Backed {
            vtable,
            ctx: provider_ctx,
        }))
    })
}

/// Return an owned lazy handle to the API-owned global LoggerProvider.
#[no_mangle]
pub extern "C" fn otel_global_logger_provider() -> *mut OtelLoggerProvider {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelLoggerProvider::new(LoggerProviderInner::Global))
    })
}
