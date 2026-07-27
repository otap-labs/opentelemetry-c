use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry_c_abi::{
    OtelKeyValue, OtelMetricInstrumentConfig, OtelMetricScopeConfig, OtelMetricsVtable, OtelStatus,
    OtelStringView, OTEL_METRICS_IMPL_ABI_VERSION,
};
use opentelemetry_c_api::{
    otel_api_register_global_meter_provider, otel_global_meter_provider, otel_meter_destroy,
    otel_meter_provider_destroy, otel_meter_provider_get_meter,
};

const LIVE_MAGIC: u64 = 0x4D45_5445_524C_4956;
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

extern "C" fn get_meter(
    ctx: *mut c_void,
    _: OtelStringView,
    _: OtelStringView,
    _: OtelStringView,
) -> *mut c_void {
    check_alive(ctx);
    Box::into_raw(Box::new(0u8)).cast()
}

extern "C" fn get_meter_with_scope(
    ctx: *mut c_void,
    _: *const OtelMetricScopeConfig,
) -> *mut c_void {
    check_alive(ctx);
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

extern "C" fn object_free(ctx: *mut c_void) {
    if !ctx.is_null() {
        drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
    }
}

extern "C" fn create(_: *mut c_void, _: *const OtelMetricInstrumentConfig) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn create_with_status(
    ctx: *mut c_void,
    config: *const OtelMetricInstrumentConfig,
    out_status: *mut OtelStatus,
) -> *mut c_void {
    if !out_status.is_null() {
        unsafe { *out_status = OtelStatus::InvalidConfig };
    }
    create(ctx, config)
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

static VTABLE: OtelMetricsVtable = OtelMetricsVtable {
    abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelMetricsVtable>(),
    provider_get_meter: get_meter,
    provider_retain: retain,
    provider_free,
    meter_create_instrument: create,
    meter_free: object_free,
    instrument_record_u64: record_u64,
    instrument_record_i64: record_i64,
    instrument_record_f64: record_f64,
    observer_observe_u64: record_u64,
    observer_observe_i64: record_i64,
    observer_observe_f64: record_f64,
    instrument_free: object_free,
    provider_get_meter_with_scope: get_meter_with_scope,
    meter_create_instrument_with_status: create_with_status,
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

#[test]
fn global_meter_provider_lifetime_is_race_free() {
    unsafe { otel_api_register_global_meter_provider(&VTABLE, new_provider_ctx()) };

    let stop = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut threads = Vec::new();

    for _ in 0..4 {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            while !stop.load(SeqCst) {
                unsafe {
                    let provider = otel_global_meter_provider();
                    let meter =
                        otel_meter_provider_get_meter(provider, sv("instr"), empty(), empty());
                    otel_meter_destroy(meter);
                    otel_meter_provider_destroy(provider);
                }
            }
        }));
    }

    for _ in 0..2 {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            while !stop.load(SeqCst) {
                unsafe {
                    otel_api_register_global_meter_provider(&VTABLE, new_provider_ctx());
                }
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
}
