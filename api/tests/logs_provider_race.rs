// SPDX-License-Identifier: Apache-2.0

//! Concurrency proof for the API-owned global LoggerProvider slot.
//!
//! The Logs slot is separate from the trace and Metrics slots, so it needs its own proof that a
//! reader can never observe a provider context that a concurrent re-registration has already
//! freed. The fake implementation below stores a magic word that is cleared on drop, so any
//! use-after-free is detected deterministically rather than relying on an allocator to notice.
//!
//! It also proves reference-count conservation across the token-based registration path:
//! every context created or retained must be freed exactly once, except the one still owned by
//! the slot when the test ends.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry_c_abi::{
    OtelBool, OtelLogRecordView, OtelLogsVtable, OtelScopeConfig, OtelStatus, OtelStringView,
    OTEL_LOGS_IMPL_ABI_VERSION,
};
use opentelemetry_c_api::{
    otel_api_register_global_logger_provider_with_token,
    otel_api_unregister_global_logger_provider, otel_global_logger_provider, otel_logger_destroy,
    otel_logger_emit, otel_logger_enabled, otel_logger_provider_destroy,
    otel_logger_provider_get_logger,
};

const LIVE_MAGIC: u64 = 0x4C4F_4753_4C49_5645;
static NEW: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
static RETAINS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);
static SAW_DEAD: AtomicBool = AtomicBool::new(false);

struct TestInner {
    magic: AtomicU64,
}

impl Drop for TestInner {
    fn drop(&mut self) {
        self.magic.store(0, SeqCst);
        DROPPED.fetch_add(1, SeqCst);
    }
}

fn new_provider_ctx() -> *mut c_void {
    NEW.fetch_add(1, SeqCst);
    Box::into_raw(Box::new(Arc::new(TestInner {
        magic: AtomicU64::new(LIVE_MAGIC),
    })))
    .cast()
}

fn check_alive(ctx: *mut c_void) {
    let provider = unsafe { &*ctx.cast::<Arc<TestInner>>() };
    if provider.magic.load(SeqCst) != LIVE_MAGIC {
        SAW_DEAD.store(true, SeqCst);
    }
}

extern "C" fn get_logger(ctx: *mut c_void, scope: *const OtelScopeConfig) -> *mut c_void {
    check_alive(ctx);
    if scope.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(0u8)).cast()
}

extern "C" fn retain(ctx: *mut c_void) -> *mut c_void {
    check_alive(ctx);
    RETAINS.fetch_add(1, SeqCst);
    let provider = unsafe { &*ctx.cast::<Arc<TestInner>>() };
    Box::into_raw(Box::new(Arc::clone(provider))).cast()
}

extern "C" fn provider_free(ctx: *mut c_void) {
    FREES.fetch_add(1, SeqCst);
    drop(unsafe { Box::from_raw(ctx.cast::<Arc<TestInner>>()) });
}

extern "C" fn logger_enabled(_: *mut c_void, severity: u32) -> OtelBool {
    u32::from(severity != 0)
}

extern "C" fn logger_emit(_: *mut c_void, record: *const OtelLogRecordView) -> OtelStatus {
    if record.is_null() {
        OtelStatus::InvalidArgument
    } else {
        OtelStatus::Ok
    }
}

extern "C" fn logger_free(ctx: *mut c_void) {
    if !ctx.is_null() {
        drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
    }
}

static VTABLE: OtelLogsVtable = OtelLogsVtable {
    abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelLogsVtable>(),
    provider_get_logger: get_logger,
    provider_retain: retain,
    provider_free,
    logger_enabled,
    logger_emit,
    logger_free,
};

fn sv(value: &'static str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

fn empty() -> OtelStringView {
    OtelStringView {
        ptr: std::ptr::null(),
        len: 0,
    }
}

fn register() -> u64 {
    let ctx = new_provider_ctx();
    let mut id = 0u64;
    let status =
        unsafe { otel_api_register_global_logger_provider_with_token(&VTABLE, ctx, &mut id) };
    assert_eq!(status, OtelStatus::Ok);
    assert_ne!(id, 0);
    id
}

fn valid_record() -> OtelLogRecordView {
    let mut record: OtelLogRecordView = unsafe { std::mem::zeroed() };
    record.struct_size = std::mem::size_of::<OtelLogRecordView>() as u64;
    record.severity_number = 9;
    record
}

#[test]
fn global_logger_provider_lifetime_is_race_free() {
    register();

    let stop = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut threads = Vec::new();

    // Readers exercise the full lazy-handle path: resolve the global slot, retain through the
    // vtable, create a logger, emit, then release everything.
    for _ in 0..4 {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            let record = valid_record();
            while !stop.load(SeqCst) {
                unsafe {
                    let provider = otel_global_logger_provider();
                    let logger =
                        otel_logger_provider_get_logger(provider, sv("instr"), empty(), empty());
                    if !logger.is_null() {
                        let _ = otel_logger_enabled(logger, 9);
                        let _ = otel_logger_emit(logger, &record);
                    }
                    otel_logger_destroy(logger);
                    otel_logger_provider_destroy(provider);
                }
            }
        }));
    }

    for _ in 0..2 {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            while !stop.load(SeqCst) {
                register();
            }
        }));
    }

    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, SeqCst);
    for thread in threads {
        thread.join().unwrap();
    }

    assert!(!SAW_DEAD.load(SeqCst), "reader observed a freed provider");
    let new = NEW.load(SeqCst);
    let retains = RETAINS.load(SeqCst);
    let frees = FREES.load(SeqCst);
    assert!(new >= 2, "expected provider replacement churn");
    assert_eq!(new + retains, frees + 1, "provider reference imbalance");
    assert_eq!(
        DROPPED.load(SeqCst),
        new - 1,
        "provider leak or double free"
    );

    // A token that was never issued must be a successful no-op rather than clearing whichever
    // provider happens to own the slot: an older SDK shutting down cannot evict a newer one.
    assert_eq!(
        otel_api_unregister_global_logger_provider(u64::MAX),
        OtelStatus::Ok
    );
    assert_eq!(FREES.load(SeqCst), frees, "stale token freed a provider");
}
