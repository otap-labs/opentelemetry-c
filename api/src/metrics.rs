//! Public OpenTelemetry Metrics API.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use opentelemetry_c_abi::{
    OtelKeyValue, OtelMetricInstrumentConfig, OtelMetricInstrumentKind, OtelMetricNumberKind,
    OtelMetricsVtable, OtelStringView,
};

use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::{
    checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasMagic,
};
use crate::metrics_global::{retain_global_metrics, GlobalMetricsRetain};

const METER_PROVIDER_MAGIC: u64 = 0x4F54_4C43_4D50_524F;
const METER_MAGIC: u64 = 0x4F54_4C43_4D45_5445;
const MAX_HISTOGRAM_BOUNDARIES: usize = 65_536;

pub(crate) enum MeterProviderInner {
    Global,
    Backed {
        vtable: *const OtelMetricsVtable,
        ctx: *mut c_void,
    },
}

pub struct OtelMeterProvider {
    magic: u64,
    inner: MeterProviderInner,
}

impl OtelMeterProvider {
    pub(crate) fn new(inner: MeterProviderInner) -> Self {
        Self {
            magic: METER_PROVIDER_MAGIC,
            inner,
        }
    }
}

impl HasMagic for OtelMeterProvider {
    const MAGIC: u64 = METER_PROVIDER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

pub struct OtelMeter {
    magic: u64,
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
}

impl HasMagic for OtelMeter {
    const MAGIC: u64 = METER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

unsafe impl Send for OtelMeterProvider {}
unsafe impl Sync for OtelMeterProvider {}
unsafe impl Send for OtelMeter {}
unsafe impl Sync for OtelMeter {}

const _: () = {
    fn assert_sync<T: Sync>() {}
    let _ = assert_sync::<OtelMeterProvider>;
    let _ = assert_sync::<OtelMeter>;
};

/// Extensible instrument creation options. `struct_size` must be initialized to
/// `sizeof(otel_instrument_options_t)`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelInstrumentOptions {
    pub struct_size: u64,
    pub description: OtelStringView,
    pub unit: OtelStringView,
    pub boundaries: *const f64,
    pub boundary_count: usize,
}

#[repr(C)]
struct OtelInstrumentOptionsV1 {
    struct_size: u64,
    description: OtelStringView,
    unit: OtelStringView,
    boundaries: *const f64,
    boundary_count: usize,
}

const OTEL_INSTRUMENT_OPTIONS_V1_SIZE: u64 = std::mem::size_of::<OtelInstrumentOptionsV1>() as u64;

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelInstrumentOptions>() == 56);
    assert!(std::mem::align_of::<OtelInstrumentOptions>() == 8);
};

fn empty_view() -> OtelStringView {
    OtelStringView::empty()
}

fn fail_abi(error: opentelemetry_c_abi::AbiError) -> OtelStatus {
    fail(error.status, error.message)
}

unsafe fn validate_instrument_config(
    name: OtelStringView,
    options: *const OtelInstrumentOptions,
    histogram: bool,
) -> Result<OtelMetricInstrumentConfig, OtelStatus> {
    let name_str = unsafe { name.as_str() }.map_err(fail_abi)?;
    if name_str.is_empty() {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "metric instrument name must not be empty",
        ));
    }
    if name_str.len() > 255 {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "metric instrument name must be at most 255 bytes",
        ));
    }
    if !name_str.as_bytes()[0].is_ascii_alphabetic()
        || !name_str
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-/".contains(&byte))
    {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "metric instrument name must start with ASCII alphabetic and contain only ASCII alphanumeric, '_', '.', '-', or '/'",
        ));
    }

    let mut description = empty_view();
    let mut unit = empty_view();
    let mut boundaries = std::ptr::null();
    let mut boundary_count = 0;
    if !options.is_null() {
        // `struct_size` is the stable prefix. Read it without forming a reference to the
        // current full structure, so a future API can safely reject a genuinely shorter V1
        // caller before accessing fields that are not present.
        let struct_size = unsafe { options.cast::<u64>().read() };
        if struct_size < OTEL_INSTRUMENT_OPTIONS_V1_SIZE {
            return Err(fail(
                OtelStatus::InvalidConfig,
                "instrument options struct_size is too small",
            ));
        }
        let options = unsafe { &*options.cast::<OtelInstrumentOptionsV1>() };
        unsafe { options.description.as_str() }.map_err(fail_abi)?;
        let unit_str = unsafe { options.unit.as_str() }.map_err(fail_abi)?;
        if unit_str.len() > 63 || !unit_str.is_ascii() {
            return Err(fail(
                OtelStatus::InvalidConfig,
                "metric instrument unit must be ASCII and at most 63 bytes",
            ));
        }
        description = options.description;
        unit = options.unit;
        boundaries = options.boundaries;
        boundary_count = options.boundary_count;
    }

    if !histogram && (boundary_count != 0 || !boundaries.is_null()) {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "histogram boundaries are only valid for histogram instruments",
        ));
    }
    if histogram {
        if boundary_count > MAX_HISTOGRAM_BOUNDARIES {
            return Err(fail(
                OtelStatus::InvalidConfig,
                "histogram boundary count exceeds the supported maximum",
            ));
        }
        if boundary_count == 0 {
            boundaries = std::ptr::null();
        } else {
            if boundaries.is_null() {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "histogram boundaries are NULL with non-zero count",
                ));
            }
            let valid_size = boundary_count
                .checked_mul(std::mem::size_of::<f64>())
                .is_some_and(|bytes| bytes <= isize::MAX as usize);
            if !valid_size {
                return Err(fail(
                    OtelStatus::InvalidArgument,
                    "histogram boundary array exceeds the maximum supported size",
                ));
            }
            let values = unsafe { std::slice::from_raw_parts(boundaries, boundary_count) };
            if values.iter().any(|value| !value.is_finite())
                || values.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(fail(
                    OtelStatus::InvalidConfig,
                    "histogram boundaries must be finite and strictly increasing",
                ));
            }
        }
    }

    Ok(OtelMetricInstrumentConfig {
        kind: 0,
        number: 0,
        name,
        description,
        unit,
        boundaries,
        boundary_count,
        callback: None,
        callback_state: std::ptr::null_mut(),
        callback_state_free: None,
    })
}

/// Obtain an owned meter from a provider.
///
/// # Safety
///
/// `provider` must be a live provider handle. Every non-empty string view must address
/// readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn otel_meter_provider_get_meter(
    provider: *const OtelMeterProvider,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut OtelMeter {
    guard_ptr(|| {
        clear_last_error();
        let provider = match unsafe { checked_ref(provider) } {
            Some(provider) => provider,
            None => return std::ptr::null_mut(),
        };
        let (vtable, ctx, owned) = match &provider.inner {
            MeterProviderInner::Global => match retain_global_metrics() {
                GlobalMetricsRetain::NoProvider => {
                    return into_raw(OtelMeter {
                        magic: METER_MAGIC,
                        vtable: std::ptr::null(),
                        ctx: std::ptr::null_mut(),
                    });
                }
                GlobalMetricsRetain::RetainFailed => return std::ptr::null_mut(),
                GlobalMetricsRetain::Retained { vtable, ctx } => (vtable, ctx, true),
            },
            MeterProviderInner::Backed { vtable, ctx } => (*vtable, *ctx, false),
        };
        let meter_ctx = unsafe { ((*vtable).provider_get_meter)(ctx, name, version, schema_url) };
        if owned {
            unsafe { ((*vtable).provider_free)(ctx) };
        }
        if meter_ctx.is_null() {
            if !has_last_error() {
                set_last_error("backed meter creation failed");
            }
            return std::ptr::null_mut();
        }
        into_raw(OtelMeter {
            magic: METER_MAGIC,
            vtable,
            ctx: meter_ctx,
        })
    })
}

/// Destroy a meter provider handle.
///
/// # Safety
///
/// `provider` must be NULL or a live provider handle, and destruction must not race with
/// another use of the same handle.
#[no_mangle]
pub unsafe extern "C" fn otel_meter_provider_destroy(provider: *mut OtelMeterProvider) {
    guard_unit(|| {
        if let Some(provider) = unsafe { checked_ref::<OtelMeterProvider>(provider) } {
            if let MeterProviderInner::Backed { vtable, ctx } = &provider.inner {
                unsafe { ((**vtable).provider_free)(*ctx) };
            }
        }
        unsafe { destroy(provider) };
    });
}

/// Destroy a meter handle.
///
/// # Safety
///
/// `meter` must be NULL or a live meter handle, and destruction must not race with another
/// use of the same handle.
#[no_mangle]
pub unsafe extern "C" fn otel_meter_destroy(meter: *mut OtelMeter) {
    guard_unit(|| {
        if let Some(meter) = unsafe { checked_ref::<OtelMeter>(meter) } {
            if !meter.vtable.is_null() {
                unsafe { ((*meter.vtable).meter_free)(meter.ctx) };
            }
        }
        unsafe { destroy(meter) };
    });
}

fn create_instrument(
    meter: &OtelMeter,
    mut config: OtelMetricInstrumentConfig,
    kind: OtelMetricInstrumentKind,
    number: OtelMetricNumberKind,
) -> Result<(*const OtelMetricsVtable, *mut c_void), OtelStatus> {
    if meter.vtable.is_null() {
        return Ok((std::ptr::null(), std::ptr::null_mut()));
    }
    config.kind = kind as u32;
    config.number = number as u32;
    let ctx = unsafe { ((*meter.vtable).meter_create_instrument)(meter.ctx, &config) };
    if ctx.is_null() {
        if !has_last_error() {
            set_last_error("backed metric instrument creation failed");
        }
        return Err(OtelStatus::InvalidConfig);
    }
    Ok((meter.vtable, ctx))
}

#[allow(unknown_lints)]
#[allow(edition_2024_expr_fragment_specifier)]
macro_rules! define_sync_instrument {
    (
        $handle:ident, $magic:expr, $create:ident, $record:ident, $destroy_fn:ident,
        $kind:ident, $number:ident, $value:ty, $vtable_record:ident, $histogram:expr
    ) => {
        pub struct $handle {
            magic: u64,
            vtable: *const OtelMetricsVtable,
            ctx: *mut c_void,
        }

        impl HasMagic for $handle {
            const MAGIC: u64 = $magic;
            fn magic(&self) -> u64 {
                self.magic
            }
            fn set_magic(&mut self, value: u64) {
                self.magic = value;
            }
        }

        unsafe impl Send for $handle {}
        unsafe impl Sync for $handle {}

        #[doc = "Create a synchronous instrument."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`meter` and optional configuration pointers must be live for the call, and \
                 `out` must address writable storage."]
        #[no_mangle]
        pub unsafe extern "C" fn $create(
            meter: *const OtelMeter,
            name: OtelStringView,
            options: *const OtelInstrumentOptions,
            out: *mut *mut $handle,
        ) -> OtelStatus {
            guard_status(|| {
                clear_last_error();
                if out.is_null() {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "instrument out pointer is NULL",
                    );
                }
                unsafe { *out = std::ptr::null_mut() };
                let meter = match unsafe { checked_ref(meter) } {
                    Some(meter) => meter,
                    None => return OtelStatus::InvalidArgument,
                };
                let config = match unsafe { validate_instrument_config(name, options, $histogram) }
                {
                    Ok(config) => config,
                    Err(status) => return status,
                };
                let (vtable, ctx) = match create_instrument(
                    meter,
                    config,
                    OtelMetricInstrumentKind::$kind,
                    OtelMetricNumberKind::$number,
                ) {
                    Ok(value) => value,
                    Err(status) => return status,
                };
                unsafe {
                    *out = into_raw($handle {
                        magic: $magic,
                        vtable,
                        ctx,
                    })
                };
                OtelStatus::Ok
            })
        }

        #[doc = "Record a synchronous measurement."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be a live handle of the exact expected type. When the \
                 attribute count is non-zero, `attributes` must address that many readable \
                 values."]
        #[no_mangle]
        pub unsafe extern "C" fn $record(
            instrument: *const $handle,
            value: $value,
            attributes: *const OtelKeyValue,
            attribute_count: usize,
        ) -> OtelStatus {
            guard_status(|| {
                clear_last_error();
                let instrument = match unsafe { checked_ref(instrument) } {
                    Some(instrument) => instrument,
                    None => return OtelStatus::InvalidArgument,
                };
                if instrument.vtable.is_null() {
                    return OtelStatus::Ok;
                }
                unsafe {
                    ((*instrument.vtable).$vtable_record)(
                        instrument.ctx,
                        value,
                        attributes,
                        attribute_count,
                    )
                }
            })
        }

        #[doc = "Destroy a synchronous instrument handle."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be NULL or a live handle of the exact expected type, and \
                 destruction must not race with another use of that handle."]
        #[no_mangle]
        pub unsafe extern "C" fn $destroy_fn(instrument: *mut $handle) {
            guard_unit(|| {
                if let Some(instrument) = unsafe { checked_ref::<$handle>(instrument) } {
                    if !instrument.vtable.is_null() {
                        unsafe { ((*instrument.vtable).instrument_free)(instrument.ctx) };
                    }
                }
                unsafe { destroy(instrument) };
            });
        }
    };
}

define_sync_instrument!(
    OtelCounterU64,
    0x4F544D4300010001,
    otel_meter_create_u64_counter,
    otel_counter_u64_add,
    otel_counter_u64_destroy,
    Counter,
    U64,
    u64,
    instrument_record_u64,
    false
);
define_sync_instrument!(
    OtelCounterF64,
    0x4F544D4300010002,
    otel_meter_create_f64_counter,
    otel_counter_f64_add,
    otel_counter_f64_destroy,
    Counter,
    F64,
    f64,
    instrument_record_f64,
    false
);
define_sync_instrument!(
    OtelUpDownCounterI64,
    0x4F544D4300020001,
    otel_meter_create_i64_up_down_counter,
    otel_up_down_counter_i64_add,
    otel_up_down_counter_i64_destroy,
    UpDownCounter,
    I64,
    i64,
    instrument_record_i64,
    false
);
define_sync_instrument!(
    OtelUpDownCounterF64,
    0x4F544D4300020002,
    otel_meter_create_f64_up_down_counter,
    otel_up_down_counter_f64_add,
    otel_up_down_counter_f64_destroy,
    UpDownCounter,
    F64,
    f64,
    instrument_record_f64,
    false
);
define_sync_instrument!(
    OtelGaugeU64,
    0x4F544D4300030001,
    otel_meter_create_u64_gauge,
    otel_gauge_u64_record,
    otel_gauge_u64_destroy,
    Gauge,
    U64,
    u64,
    instrument_record_u64,
    false
);
define_sync_instrument!(
    OtelGaugeI64,
    0x4F544D4300030002,
    otel_meter_create_i64_gauge,
    otel_gauge_i64_record,
    otel_gauge_i64_destroy,
    Gauge,
    I64,
    i64,
    instrument_record_i64,
    false
);
define_sync_instrument!(
    OtelGaugeF64,
    0x4F544D4300030003,
    otel_meter_create_f64_gauge,
    otel_gauge_f64_record,
    otel_gauge_f64_destroy,
    Gauge,
    F64,
    f64,
    instrument_record_f64,
    false
);
define_sync_instrument!(
    OtelHistogramU64,
    0x4F544D4300040001,
    otel_meter_create_u64_histogram,
    otel_histogram_u64_record,
    otel_histogram_u64_destroy,
    Histogram,
    U64,
    u64,
    instrument_record_u64,
    true
);
define_sync_instrument!(
    OtelHistogramF64,
    0x4F544D4300040002,
    otel_meter_create_f64_histogram,
    otel_histogram_f64_record,
    otel_histogram_f64_destroy,
    Histogram,
    F64,
    f64,
    instrument_record_f64,
    true
);

pub enum OtelObserverU64 {}
pub enum OtelObserverI64 {}
pub enum OtelObserverF64 {}

pub type OtelObservableCallbackU64 =
    Option<extern "C" fn(observer: *mut OtelObserverU64, user_data: *mut c_void)>;
pub type OtelObservableCallbackI64 =
    Option<extern "C" fn(observer: *mut OtelObserverI64, user_data: *mut c_void)>;
pub type OtelObservableCallbackF64 =
    Option<extern "C" fn(observer: *mut OtelObserverF64, user_data: *mut c_void)>;
pub type OtelUserDataDestroy = Option<extern "C" fn(user_data: *mut c_void)>;

enum UserCallback {
    U64(extern "C" fn(*mut OtelObserverU64, *mut c_void)),
    I64(extern "C" fn(*mut OtelObserverI64, *mut c_void)),
    F64(extern "C" fn(*mut OtelObserverF64, *mut c_void)),
}

struct CallbackState {
    enabled: AtomicBool,
    vtable: *const OtelMetricsVtable,
    callback: UserCallback,
    user_data: Mutex<Option<Arc<UserData>>>,
}

unsafe impl Send for CallbackState {}
unsafe impl Sync for CallbackState {}

struct UserData {
    ptr: *mut c_void,
    destroy: OtelUserDataDestroy,
}

unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

impl Drop for UserData {
    fn drop(&mut self) {
        if let Some(destroy) = self.destroy {
            destroy(self.ptr);
        }
    }
}

impl CallbackState {
    fn acquire_user_data(&self) -> Option<Arc<UserData>> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        let user_data = self
            .user_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        user_data.as_ref().map(Arc::clone)
    }

    fn disable_and_release_user_data(&self) {
        self.enabled.store(false, Ordering::Release);
        let user_data = self
            .user_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        drop(user_data);
    }
}

struct ObserverEntry {
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
    number: OtelMetricNumberKind,
}

unsafe impl Send for ObserverEntry {}

static OBSERVERS: OnceLock<Mutex<HashMap<usize, ObserverEntry>>> = OnceLock::new();
static NEXT_OBSERVER: AtomicUsize = AtomicUsize::new(1);

fn observer_registry() -> &'static Mutex<HashMap<usize, ObserverEntry>> {
    OBSERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ObserverRegistration {
    token: usize,
}

impl ObserverRegistration {
    fn new(
        vtable: *const OtelMetricsVtable,
        ctx: *mut c_void,
        number: OtelMetricNumberKind,
    ) -> Self {
        let token = NEXT_OBSERVER.fetch_add(1, Ordering::Relaxed).max(1);
        observer_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                token,
                ObserverEntry {
                    vtable,
                    ctx,
                    number,
                },
            );
        Self { token }
    }
}

impl Drop for ObserverRegistration {
    fn drop(&mut self) {
        observer_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.token);
    }
}

extern "C" fn callback_state_free(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Arc::from_raw(state as *const CallbackState) });
    }
}

fn callback_state_clone(state: *mut c_void) -> Option<Arc<CallbackState>> {
    if state.is_null() {
        return None;
    }
    let ptr = state as *const CallbackState;
    unsafe { Arc::increment_strong_count(ptr) };
    Some(unsafe { Arc::from_raw(ptr) })
}

extern "C" fn callback_trampoline_u64(observer_ctx: *mut c_void, state: *mut c_void) {
    guard_unit(|| {
        let Some(state) = callback_state_clone(state) else {
            return;
        };
        let Some(user_data) = state.acquire_user_data() else {
            return;
        };
        let registration =
            ObserverRegistration::new(state.vtable, observer_ctx, OtelMetricNumberKind::U64);
        if let UserCallback::U64(callback) = state.callback {
            callback(registration.token as *mut OtelObserverU64, user_data.ptr);
        }
    });
}

extern "C" fn callback_trampoline_i64(observer_ctx: *mut c_void, state: *mut c_void) {
    guard_unit(|| {
        let Some(state) = callback_state_clone(state) else {
            return;
        };
        let Some(user_data) = state.acquire_user_data() else {
            return;
        };
        let registration =
            ObserverRegistration::new(state.vtable, observer_ctx, OtelMetricNumberKind::I64);
        if let UserCallback::I64(callback) = state.callback {
            callback(registration.token as *mut OtelObserverI64, user_data.ptr);
        }
    });
}

extern "C" fn callback_trampoline_f64(observer_ctx: *mut c_void, state: *mut c_void) {
    guard_unit(|| {
        let Some(state) = callback_state_clone(state) else {
            return;
        };
        let Some(user_data) = state.acquire_user_data() else {
            return;
        };
        let registration =
            ObserverRegistration::new(state.vtable, observer_ctx, OtelMetricNumberKind::F64);
        if let UserCallback::F64(callback) = state.callback {
            callback(registration.token as *mut OtelObserverF64, user_data.ptr);
        }
    });
}

fn observe<T>(
    token: usize,
    expected: OtelMetricNumberKind,
    value: T,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
    call: impl FnOnce(&ObserverEntry, T) -> OtelStatus,
) -> OtelStatus {
    let registry = observer_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = match registry.get(&token) {
        Some(entry) if entry.number == expected && !entry.vtable.is_null() => entry,
        _ => {
            return fail(
                OtelStatus::InvalidArgument,
                "observer is not active for this callback invocation",
            )
        }
    };
    let _ = (attributes, attribute_count);
    call(entry, value)
}

/// Record a value through a callback-scoped unsigned observer.
///
/// # Safety
///
/// `observer` must be the token passed to the currently executing callback. When
/// `attribute_count` is non-zero, `attributes` must address that many readable values.
#[no_mangle]
pub unsafe extern "C" fn otel_observer_u64_observe(
    observer: *mut OtelObserverU64,
    value: u64,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        observe(
            observer as usize,
            OtelMetricNumberKind::U64,
            value,
            attributes,
            attribute_count,
            |entry, value| unsafe {
                ((*entry.vtable).observer_observe_u64)(
                    entry.ctx,
                    value,
                    attributes,
                    attribute_count,
                )
            },
        )
    })
}

/// Record a value through a callback-scoped signed observer.
///
/// # Safety
///
/// `observer` must be the token passed to the currently executing callback. When
/// `attribute_count` is non-zero, `attributes` must address that many readable values.
#[no_mangle]
pub unsafe extern "C" fn otel_observer_i64_observe(
    observer: *mut OtelObserverI64,
    value: i64,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        observe(
            observer as usize,
            OtelMetricNumberKind::I64,
            value,
            attributes,
            attribute_count,
            |entry, value| unsafe {
                ((*entry.vtable).observer_observe_i64)(
                    entry.ctx,
                    value,
                    attributes,
                    attribute_count,
                )
            },
        )
    })
}

/// Record a value through a callback-scoped floating-point observer.
///
/// # Safety
///
/// `observer` must be the token passed to the currently executing callback. When
/// `attribute_count` is non-zero, `attributes` must address that many readable values.
#[no_mangle]
pub unsafe extern "C" fn otel_observer_f64_observe(
    observer: *mut OtelObserverF64,
    value: f64,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        observe(
            observer as usize,
            OtelMetricNumberKind::F64,
            value,
            attributes,
            attribute_count,
            |entry, value| unsafe {
                ((*entry.vtable).observer_observe_f64)(
                    entry.ctx,
                    value,
                    attributes,
                    attribute_count,
                )
            },
        )
    })
}

fn create_observable(
    meter: &OtelMeter,
    mut config: OtelMetricInstrumentConfig,
    kind: OtelMetricInstrumentKind,
    number: OtelMetricNumberKind,
    state: &Arc<CallbackState>,
    trampoline: extern "C" fn(*mut c_void, *mut c_void),
) -> Result<(*const OtelMetricsVtable, *mut c_void), OtelStatus> {
    if meter.vtable.is_null() {
        return Ok((std::ptr::null(), std::ptr::null_mut()));
    }
    config.kind = kind as u32;
    config.number = number as u32;
    config.callback = Some(trampoline);
    config.callback_state = Arc::into_raw(Arc::clone(state)) as *mut c_void;
    config.callback_state_free = Some(callback_state_free);
    let ctx = unsafe { ((*meter.vtable).meter_create_instrument)(meter.ctx, &config) };
    if ctx.is_null() {
        if !has_last_error() {
            set_last_error("backed observable metric instrument creation failed");
        }
        return Err(OtelStatus::InvalidConfig);
    }
    Ok((meter.vtable, ctx))
}

#[allow(unknown_lints)]
#[allow(edition_2024_expr_fragment_specifier)]
macro_rules! define_observable_instrument {
    (
        $handle:ident, $magic:expr, $create:ident, $destroy_fn:ident,
        $kind:ident, $number:ident, $callback_ty:ty, $callback_variant:ident,
        $trampoline:ident
    ) => {
        pub struct $handle {
            magic: u64,
            vtable: *const OtelMetricsVtable,
            ctx: *mut c_void,
            state: Arc<CallbackState>,
        }

        impl HasMagic for $handle {
            const MAGIC: u64 = $magic;
            fn magic(&self) -> u64 {
                self.magic
            }
            fn set_magic(&mut self, value: u64) {
                self.magic = value;
            }
        }

        unsafe impl Send for $handle {}
        unsafe impl Sync for $handle {}

        #[doc = "Create an observable instrument."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`meter` and optional configuration pointers must be live for the call. \
                 `out` must be writable. Callback user data must satisfy the documented C \
                 ownership contract."]
        #[no_mangle]
        pub unsafe extern "C" fn $create(
            meter: *const OtelMeter,
            name: OtelStringView,
            options: *const OtelInstrumentOptions,
            callback: $callback_ty,
            user_data: *mut c_void,
            user_data_destroy: OtelUserDataDestroy,
            out: *mut *mut $handle,
        ) -> OtelStatus {
            guard_status(|| {
                clear_last_error();
                if out.is_null() {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "instrument out pointer is NULL",
                    );
                }
                unsafe { *out = std::ptr::null_mut() };
                let callback = match callback {
                    Some(callback) => callback,
                    None => {
                        return fail(
                            OtelStatus::InvalidArgument,
                            "observable callback must not be NULL",
                        )
                    }
                };
                let meter = match unsafe { checked_ref(meter) } {
                    Some(meter) => meter,
                    None => return OtelStatus::InvalidArgument,
                };
                let config = match unsafe { validate_instrument_config(name, options, false) } {
                    Ok(config) => config,
                    Err(status) => return status,
                };
                let state = Arc::new(CallbackState {
                    enabled: AtomicBool::new(true),
                    vtable: meter.vtable,
                    callback: UserCallback::$callback_variant(callback),
                    user_data: Mutex::new(Some(Arc::new(UserData {
                        ptr: user_data,
                        destroy: user_data_destroy,
                    }))),
                });
                let (vtable, ctx) = match create_observable(
                    meter,
                    config,
                    OtelMetricInstrumentKind::$kind,
                    OtelMetricNumberKind::$number,
                    &state,
                    $trampoline,
                ) {
                    Ok(value) => value,
                    Err(status) => return status,
                };
                unsafe {
                    *out = into_raw($handle {
                        magic: $magic,
                        vtable,
                        ctx,
                        state,
                    })
                };
                OtelStatus::Ok
            })
        }

        #[doc = "Destroy an observable instrument handle."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be NULL or a live handle of the exact expected type, and \
                 destruction must not race with another use of that handle."]
        #[no_mangle]
        pub unsafe extern "C" fn $destroy_fn(instrument: *mut $handle) {
            guard_unit(|| {
                if let Some(instrument) = unsafe { checked_ref::<$handle>(instrument) } {
                    instrument.state.disable_and_release_user_data();
                    if !instrument.vtable.is_null() {
                        unsafe { ((*instrument.vtable).instrument_free)(instrument.ctx) };
                    }
                }
                unsafe { destroy(instrument) };
            });
        }
    };
}

define_observable_instrument!(
    OtelObservableCounterU64,
    0x4F544D4301010001,
    otel_meter_create_u64_observable_counter,
    otel_observable_counter_u64_destroy,
    ObservableCounter,
    U64,
    OtelObservableCallbackU64,
    U64,
    callback_trampoline_u64
);
define_observable_instrument!(
    OtelObservableCounterF64,
    0x4F544D4301010002,
    otel_meter_create_f64_observable_counter,
    otel_observable_counter_f64_destroy,
    ObservableCounter,
    F64,
    OtelObservableCallbackF64,
    F64,
    callback_trampoline_f64
);
define_observable_instrument!(
    OtelObservableUpDownCounterI64,
    0x4F544D4301020001,
    otel_meter_create_i64_observable_up_down_counter,
    otel_observable_up_down_counter_i64_destroy,
    ObservableUpDownCounter,
    I64,
    OtelObservableCallbackI64,
    I64,
    callback_trampoline_i64
);
define_observable_instrument!(
    OtelObservableUpDownCounterF64,
    0x4F544D4301020002,
    otel_meter_create_f64_observable_up_down_counter,
    otel_observable_up_down_counter_f64_destroy,
    ObservableUpDownCounter,
    F64,
    OtelObservableCallbackF64,
    F64,
    callback_trampoline_f64
);
define_observable_instrument!(
    OtelObservableGaugeU64,
    0x4F544D4301030001,
    otel_meter_create_u64_observable_gauge,
    otel_observable_gauge_u64_destroy,
    ObservableGauge,
    U64,
    OtelObservableCallbackU64,
    U64,
    callback_trampoline_u64
);
define_observable_instrument!(
    OtelObservableGaugeI64,
    0x4F544D4301030002,
    otel_meter_create_i64_observable_gauge,
    otel_observable_gauge_i64_destroy,
    ObservableGauge,
    I64,
    OtelObservableCallbackI64,
    I64,
    callback_trampoline_i64
);
define_observable_instrument!(
    OtelObservableGaugeF64,
    0x4F544D4301030003,
    otel_meter_create_f64_observable_gauge,
    otel_observable_gauge_f64_destroy,
    ObservableGauge,
    F64,
    OtelObservableCallbackF64,
    F64,
    callback_trampoline_f64
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex, OnceLock};
    use std::time::Duration;

    use opentelemetry_c_abi::OTEL_IMPL_ABI_VERSION;

    struct MockInstrument {
        callback: extern "C" fn(*mut c_void, *mut c_void),
        state: *mut c_void,
        state_free: extern "C" fn(*mut c_void),
    }

    fn free_reached() -> &'static (Mutex<bool>, Condvar) {
        static FREE_REACHED: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
        FREE_REACHED.get_or_init(|| (Mutex::new(false), Condvar::new()))
    }

    extern "C" fn mock_provider_get_meter(
        _provider_ctx: *mut c_void,
        _name: OtelStringView,
        _version: OtelStringView,
        _schema_url: OtelStringView,
    ) -> *mut c_void {
        std::ptr::NonNull::<c_void>::dangling().as_ptr()
    }

    extern "C" fn mock_provider_retain(provider_ctx: *mut c_void) -> *mut c_void {
        provider_ctx
    }

    extern "C" fn mock_free(_ctx: *mut c_void) {}

    extern "C" fn mock_meter_create_instrument(
        _meter_ctx: *mut c_void,
        config: *const OtelMetricInstrumentConfig,
    ) -> *mut c_void {
        let config = unsafe { &*config };
        let Some(callback) = config.callback else {
            return std::ptr::null_mut();
        };
        let Some(state_free) = config.callback_state_free else {
            return std::ptr::null_mut();
        };
        Box::into_raw(Box::new(MockInstrument {
            callback,
            state: config.callback_state,
            state_free,
        }))
        .cast()
    }

    extern "C" fn mock_record_u64(
        _ctx: *mut c_void,
        _value: u64,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
    ) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn mock_record_i64(
        _ctx: *mut c_void,
        _value: i64,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
    ) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn mock_record_f64(
        _ctx: *mut c_void,
        _value: f64,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
    ) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn mock_instrument_free(ctx: *mut c_void) {
        let instrument = unsafe { Box::from_raw(ctx.cast::<MockInstrument>()) };
        (instrument.state_free)(instrument.state);
        let (reached, condition) = free_reached();
        *reached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        condition.notify_all();
    }

    static MOCK_METRICS_VTABLE: OtelMetricsVtable = OtelMetricsVtable {
        abi_version: OTEL_IMPL_ABI_VERSION,
        struct_size: std::mem::size_of::<OtelMetricsVtable>(),
        provider_get_meter: mock_provider_get_meter,
        provider_retain: mock_provider_retain,
        provider_free: mock_free,
        meter_create_instrument: mock_meter_create_instrument,
        meter_free: mock_free,
        instrument_record_u64: mock_record_u64,
        instrument_record_i64: mock_record_i64,
        instrument_record_f64: mock_record_f64,
        observer_observe_u64: mock_record_u64,
        observer_observe_i64: mock_record_i64,
        observer_observe_f64: mock_record_f64,
        instrument_free: mock_instrument_free,
    };

    struct InFlightUserData {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        completed: Arc<AtomicUsize>,
        destroyed: Arc<AtomicUsize>,
    }

    extern "C" fn in_flight_callback(observer: *mut OtelObserverU64, user_data: *mut c_void) {
        let state = unsafe { &*(user_data.cast::<InFlightUserData>()) };
        state.entered.wait();
        state.release.wait();
        assert_eq!(
            unsafe { otel_observer_u64_observe(observer, 23, std::ptr::null(), 0) },
            OtelStatus::Ok
        );
        state.completed.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn in_flight_user_data_destroy(user_data: *mut c_void) {
        let state = unsafe { Box::from_raw(user_data.cast::<InFlightUserData>()) };
        state.destroyed.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn observable_destroy_defers_user_data_until_in_flight_callback_completes() {
        let (reached, _) = free_reached();
        *reached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;

        let meter = OtelMeter {
            magic: METER_MAGIC,
            vtable: &MOCK_METRICS_VTABLE,
            ctx: std::ptr::NonNull::<c_void>::dangling().as_ptr(),
        };
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let completed = Arc::new(AtomicUsize::new(0));
        let destroyed = Arc::new(AtomicUsize::new(0));
        let user_data = Box::into_raw(Box::new(InFlightUserData {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            completed: Arc::clone(&completed),
            destroyed: Arc::clone(&destroyed),
        }))
        .cast();
        let mut observable = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_meter_create_u64_observable_gauge(
                    &meter,
                    OtelStringView {
                        ptr: b"observable".as_ptr().cast(),
                        len: 10,
                    },
                    std::ptr::null(),
                    Some(in_flight_callback),
                    user_data,
                    Some(in_flight_user_data_destroy),
                    &mut observable,
                )
            },
            OtelStatus::Ok
        );

        let instrument = unsafe { &*((*observable).ctx.cast::<MockInstrument>()) };
        let callback = instrument.callback;
        let callback_state = instrument.state as usize;
        let callback_thread = std::thread::spawn(move || {
            callback(
                std::ptr::NonNull::<c_void>::dangling().as_ptr(),
                callback_state as *mut c_void,
            );
        });
        entered.wait();

        let observable_addr = observable as usize;
        let (destroy_done_tx, destroy_done_rx) = mpsc::channel();
        let destroy_thread = std::thread::spawn(move || {
            unsafe {
                otel_observable_gauge_u64_destroy(observable_addr as *mut OtelObservableGaugeU64)
            };
            destroy_done_tx.send(()).unwrap();
        });

        let (reached, condition) = free_reached();
        let guard = reached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = condition
            .wait_timeout_while(guard, Duration::from_secs(5), |reached| !*reached)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(*guard, "instrument destruction did not reach the SDK");
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        destroy_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("observable destruction did not complete");

        release.wait();
        callback_thread.join().unwrap();
        destroy_thread.join().unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }
}
