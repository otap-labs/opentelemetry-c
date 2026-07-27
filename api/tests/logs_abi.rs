//! Logs implementation-ABI contract: vtable validation, token lifecycle, and forwarding.
//!
//! Uses a hand-written in-test vtable rather than the SDK so the API's side of the internal
//! ABI is exercised in isolation.

use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use opentelemetry_c_abi::{
    OtelLogsVtable, OtelScopeConfig, OTEL_LOGS_IMPL_ABI_VERSION, OTEL_LOGS_VTABLE_REQUIRED_SIZE,
    OTEL_METRICS_IMPL_ABI_VERSION,
};
use opentelemetry_c_api as api;
use opentelemetry_c_api::{
    OtelBool, OtelLogRecordView, OtelLogTraceContext, OtelLogValue, OtelLogValuePayload,
    OtelLogValueType, OtelStatus, OtelStringView,
};

static PROVIDER_RETAINS: AtomicU32 = AtomicU32::new(0);
static PROVIDER_FREES: AtomicU32 = AtomicU32::new(0);
static LOGGERS_CREATED: AtomicU32 = AtomicU32::new(0);
static LOGGER_FREES: AtomicU32 = AtomicU32::new(0);
static EMITS: AtomicU32 = AtomicU32::new(0);
static ENABLED_CALLS: AtomicU32 = AtomicU32::new(0);
static LAST_SEVERITY: AtomicU32 = AtomicU32::new(0);
static LAST_SCOPE: Mutex<Option<(String, String, String, usize)>> = Mutex::new(None);
/// Serializes tests that observe the shared reference counters or the process-global slot.
static SERIAL: Mutex<()> = Mutex::new(());

const PROVIDER_TAG: u64 = 0xB1B1_B1B1_B1B1_B1B1;
const LOGGER_TAG: u64 = 0xC2C2_C2C2_C2C2_C2C2;

fn new_provider_ctx() -> *mut c_void {
    Box::into_raw(Box::new(PROVIDER_TAG)).cast()
}

extern "C" fn provider_get_logger(
    provider_ctx: *mut c_void,
    scope: *const OtelScopeConfig,
) -> *mut c_void {
    assert!(!provider_ctx.is_null());
    assert_eq!(unsafe { *provider_ctx.cast::<u64>() }, PROVIDER_TAG);
    assert!(!scope.is_null());
    let scope = unsafe { &*scope };
    let read = |v: OtelStringView| unsafe { v.as_str() }.expect("valid utf-8").to_owned();
    *LAST_SCOPE.lock().unwrap() = Some((
        read(scope.name),
        read(scope.version),
        read(scope.schema_url),
        scope.attribute_count,
    ));
    LOGGERS_CREATED.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(LOGGER_TAG)).cast()
}

extern "C" fn provider_retain(provider_ctx: *mut c_void) -> *mut c_void {
    assert_eq!(unsafe { *provider_ctx.cast::<u64>() }, PROVIDER_TAG);
    PROVIDER_RETAINS.fetch_add(1, Ordering::SeqCst);
    new_provider_ctx()
}

extern "C" fn provider_free(provider_ctx: *mut c_void) {
    assert_eq!(unsafe { *provider_ctx.cast::<u64>() }, PROVIDER_TAG);
    PROVIDER_FREES.fetch_add(1, Ordering::SeqCst);
    drop(unsafe { Box::from_raw(provider_ctx.cast::<u64>()) });
}

extern "C" fn logger_enabled(logger_ctx: *mut c_void, severity: u32) -> OtelBool {
    assert_eq!(unsafe { *logger_ctx.cast::<u64>() }, LOGGER_TAG);
    ENABLED_CALLS.fetch_add(1, Ordering::SeqCst);
    LAST_SEVERITY.store(severity, Ordering::SeqCst);
    u32::from(severity >= 9)
}

extern "C" fn logger_emit(logger_ctx: *mut c_void, record: *const OtelLogRecordView) -> OtelStatus {
    assert_eq!(unsafe { *logger_ctx.cast::<u64>() }, LOGGER_TAG);
    assert!(!record.is_null());
    // The API must forward the record untouched, including a larger caller struct_size.
    assert!(unsafe { (*record).struct_size } >= std::mem::size_of::<OtelLogRecordView>() as u64);
    EMITS.fetch_add(1, Ordering::SeqCst);
    OtelStatus::Ok
}

extern "C" fn logger_free(logger_ctx: *mut c_void) {
    assert_eq!(unsafe { *logger_ctx.cast::<u64>() }, LOGGER_TAG);
    LOGGER_FREES.fetch_add(1, Ordering::SeqCst);
    drop(unsafe { Box::from_raw(logger_ctx.cast::<u64>()) });
}

fn vtable() -> OtelLogsVtable {
    OtelLogsVtable {
        abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
        struct_size: std::mem::size_of::<OtelLogsVtable>(),
        provider_get_logger,
        provider_retain,
        provider_free,
        logger_enabled,
        logger_emit,
        logger_free,
    }
}

static VTABLE: OtelLogsVtable = OtelLogsVtable {
    abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelLogsVtable>(),
    provider_get_logger,
    provider_retain,
    provider_free,
    logger_enabled,
    logger_emit,
    logger_free,
};

fn sv(s: &str) -> OtelStringView {
    OtelStringView {
        ptr: s.as_ptr().cast::<c_char>(),
        len: s.len(),
    }
}

fn record() -> OtelLogRecordView {
    OtelLogRecordView {
        struct_size: std::mem::size_of::<OtelLogRecordView>() as u64,
        present_fields: 0,
        timestamp_unix_nanos: 0,
        observed_timestamp_unix_nanos: 0,
        severity_number: 9,
        reserved_flags: 0,
        body: OtelLogValue {
            value_type: OtelLogValueType::Empty as u32,
            reserved: 0,
            value: OtelLogValuePayload {
                string_value: OtelStringView::empty(),
            },
        },
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

#[test]
fn incompatible_vtables_are_rejected_before_any_slot_is_read() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    // Deliberately NULL function slots: a rejected vtable must never be called.
    #[repr(C)]
    struct HeaderOnly {
        abi_version: u32,
        struct_size: usize,
        rest: [usize; 6],
    }

    let mut out_id = 0xFFFF_FFFF_FFFF_FFFFu64;
    assert_eq!(
        unsafe {
            api::otel_api_register_global_logger_provider_with_token(
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut out_id,
            )
        },
        OtelStatus::InvalidArgument
    );
    assert_eq!(out_id, 0, "out_id must be zeroed before validation");

    let good = vtable();
    assert_eq!(
        unsafe {
            api::otel_api_register_global_logger_provider_with_token(
                &good,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        OtelStatus::InvalidArgument
    );

    for header in [
        HeaderOnly {
            abi_version: OTEL_LOGS_IMPL_ABI_VERSION.wrapping_add(1),
            struct_size: OTEL_LOGS_VTABLE_REQUIRED_SIZE,
            rest: [0; 6],
        },
        // A Metrics vtable must not be accepted through the Logs slot.
        HeaderOnly {
            abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
            struct_size: OTEL_LOGS_VTABLE_REQUIRED_SIZE,
            rest: [0; 6],
        },
        // Right kind, truncated table.
        HeaderOnly {
            abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
            struct_size: OTEL_LOGS_VTABLE_REQUIRED_SIZE - 1,
            rest: [0; 6],
        },
    ] {
        let ptr = (&header as *const HeaderOnly).cast::<OtelLogsVtable>();
        let mut out_id = 1;
        assert_eq!(
            unsafe {
                api::otel_api_register_global_logger_provider_with_token(
                    ptr,
                    std::ptr::null_mut(),
                    &mut out_id,
                )
            },
            OtelStatus::InvalidConfig
        );
        assert_eq!(out_id, 0);
        assert!(unsafe { api::otel_api_logger_provider_new(ptr, std::ptr::null_mut()) }.is_null());
    }

    assert!(
        unsafe { api::otel_api_logger_provider_new(std::ptr::null(), std::ptr::null_mut()) }
            .is_null()
    );
    assert_eq!(
        api::otel_api_unregister_global_logger_provider(0),
        OtelStatus::InvalidArgument
    );
}

#[test]
fn backed_provider_handles_own_exactly_one_reference() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let before_frees = PROVIDER_FREES.load(Ordering::SeqCst);
    let provider = unsafe { api::otel_api_logger_provider_new(&VTABLE, new_provider_ctx()) };
    assert!(!provider.is_null());

    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            provider,
            sv("scope"),
            sv("2.1"),
            sv("https://example.test/s"),
        )
    };
    assert!(!logger.is_null());
    let scope = LAST_SCOPE.lock().unwrap().clone().expect("scope captured");
    assert_eq!(
        scope,
        (
            "scope".to_owned(),
            "2.1".to_owned(),
            "https://example.test/s".to_owned(),
            0
        )
    );
    // A backed provider does not retain per logger acquisition; it lends its own reference.
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), before_frees);

    let before_logger_frees = LOGGER_FREES.load(Ordering::SeqCst);
    unsafe { api::otel_logger_destroy(logger) };
    assert_eq!(
        LOGGER_FREES.load(Ordering::SeqCst),
        before_logger_frees + 1,
        "logger context must be freed exactly once"
    );

    unsafe { api::otel_logger_provider_destroy(provider) };
    assert_eq!(
        PROVIDER_FREES.load(Ordering::SeqCst),
        before_frees + 1,
        "provider context must be freed exactly once"
    );
}

#[test]
fn global_registration_forwards_and_unregisters_by_token() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut token = 0u64;
    let ctx = new_provider_ctx();
    assert_eq!(
        unsafe {
            api::otel_api_register_global_logger_provider_with_token(&VTABLE, ctx, &mut token)
        },
        OtelStatus::Ok
    );
    assert_ne!(token, 0);

    // A lazy global handle resolves at logger-acquisition time and balances retain/free.
    let global = api::otel_global_logger_provider();
    let retains = PROVIDER_RETAINS.load(Ordering::SeqCst);
    let frees = PROVIDER_FREES.load(Ordering::SeqCst);
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            global,
            sv("global-scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };
    assert!(!logger.is_null());
    assert_eq!(PROVIDER_RETAINS.load(Ordering::SeqCst), retains + 1);
    assert_eq!(
        PROVIDER_FREES.load(Ordering::SeqCst),
        frees + 1,
        "the acquisition reference must be released before returning"
    );

    // Forwarding.
    assert_eq!(unsafe { api::otel_logger_enabled(logger, 9) }, 1);
    assert_eq!(LAST_SEVERITY.load(Ordering::SeqCst), 9);
    assert_eq!(unsafe { api::otel_logger_enabled(logger, 1) }, 0);
    // Out-of-range severities must never reach the implementation.
    let calls = ENABLED_CALLS.load(Ordering::SeqCst);
    assert_eq!(unsafe { api::otel_logger_enabled(logger, 0) }, 0);
    assert_eq!(unsafe { api::otel_logger_enabled(logger, 25) }, 0);
    assert_eq!(ENABLED_CALLS.load(Ordering::SeqCst), calls);

    let emits = EMITS.load(Ordering::SeqCst);
    let mut view = record();
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &view) },
        OtelStatus::Ok
    );
    view.struct_size += 64;
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &view) },
        OtelStatus::Ok
    );
    assert_eq!(EMITS.load(Ordering::SeqCst), emits + 2);
    // An undersized record is rejected in the API and never forwarded.
    view.struct_size = 8;
    assert_eq!(
        unsafe { api::otel_logger_emit(logger, &view) },
        OtelStatus::InvalidArgument
    );
    assert_eq!(EMITS.load(Ordering::SeqCst), emits + 2);

    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(global) };

    // A stale token is a successful no-op and must not clear the live slot.
    let frees = PROVIDER_FREES.load(Ordering::SeqCst);
    assert_eq!(
        api::otel_api_unregister_global_logger_provider(token.wrapping_add(1)),
        OtelStatus::Ok
    );
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), frees);

    // Re-registration releases the previous slot reference exactly once.
    let mut token2 = 0u64;
    assert_eq!(
        unsafe {
            api::otel_api_register_global_logger_provider_with_token(
                &VTABLE,
                new_provider_ctx(),
                &mut token2,
            )
        },
        OtelStatus::Ok
    );
    assert_ne!(token2, token);
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), frees + 1);

    // The superseded token must not clear the newer provider.
    assert_eq!(
        api::otel_api_unregister_global_logger_provider(token),
        OtelStatus::Ok
    );
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), frees + 1);

    assert_eq!(
        api::otel_api_unregister_global_logger_provider(token2),
        OtelStatus::Ok
    );
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), frees + 2);
    // Repeat unregistration of an already-cleared token is a no-op.
    assert_eq!(
        api::otel_api_unregister_global_logger_provider(token2),
        OtelStatus::Ok
    );
    assert_eq!(PROVIDER_FREES.load(Ordering::SeqCst), frees + 2);

    // After unregistration the global path is the no-op logger again.
    let global = api::otel_global_logger_provider();
    let logger = unsafe {
        api::otel_logger_provider_get_logger(
            global,
            sv("after"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        )
    };
    assert!(!logger.is_null());
    assert_eq!(unsafe { api::otel_logger_enabled(logger, 9) }, 0);
    unsafe { api::otel_logger_destroy(logger) };
    unsafe { api::otel_logger_provider_destroy(global) };
}
