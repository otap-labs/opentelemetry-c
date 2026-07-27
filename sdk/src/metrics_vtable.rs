//! SDK implementation of the internal Metrics vtable.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use opentelemetry::metrics::{
    AsyncInstrument, BoundCounter, BoundHistogram, Counter, Gauge, Histogram, Meter, MeterProvider,
    UpDownCounter,
};
use opentelemetry::InstrumentationScope;
use opentelemetry_c_abi::{
    OtelMetricInstrumentConfig, OtelMetricInstrumentKind, OtelMetricNumberKind,
    OtelMetricScopeConfig, OtelMetricsVtable, OtelStatus, OtelStringView,
    OTEL_METRICS_IMPL_ABI_VERSION,
};
use opentelemetry_sdk::metrics::SdkMeterProvider;

use crate::error::{fail, fail_abi, last_status_or, reset_last_status};

enum SdkMetricInstrument {
    CounterU64(Counter<u64>),
    CounterF64(Counter<f64>),
    UpDownCounterI64(UpDownCounter<i64>),
    UpDownCounterF64(UpDownCounter<f64>),
    GaugeU64(Gauge<u64>),
    GaugeI64(Gauge<i64>),
    GaugeF64(Gauge<f64>),
    HistogramU64(Histogram<u64>),
    HistogramF64(Histogram<f64>),
    Observable,
}

enum SdkBoundMetricInstrument {
    CounterU64(BoundCounter<u64>),
    CounterF64(BoundCounter<f64>),
    HistogramU64(BoundHistogram<u64>),
    HistogramF64(BoundHistogram<f64>),
}

struct CallbackLease {
    state: *mut c_void,
    release: extern "C" fn(*mut c_void),
}

unsafe impl Send for CallbackLease {}
unsafe impl Sync for CallbackLease {}

impl Drop for CallbackLease {
    fn drop(&mut self) {
        (self.release)(self.state);
    }
}

struct ObserverCtxU64<'a> {
    observer: &'a dyn AsyncInstrument<u64>,
}

struct ObserverCtxI64<'a> {
    observer: &'a dyn AsyncInstrument<i64>,
}

struct ObserverCtxF64<'a> {
    observer: &'a dyn AsyncInstrument<f64>,
}

fn guard_ptr(f: impl FnOnce() -> *mut c_void) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(std::ptr::null_mut())
}

fn guard_status(f: impl FnOnce() -> OtelStatus) -> OtelStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(OtelStatus::InternalError)
}

fn guard_unit(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

fn build_meter(ctx: *mut c_void, scope: &OtelMetricScopeConfig) -> *mut c_void {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let provider = unsafe { &*(ctx as *const SdkMeterProvider) };
    let name = match unsafe { scope.name.to_string_strict() } {
        Ok(name) => name,
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    };
    let mut builder = InstrumentationScope::builder(name);
    match unsafe { scope.version.to_string_strict() } {
        Ok(version) if !version.is_empty() => builder = builder.with_version(version),
        Ok(_) => {}
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    }
    match unsafe { scope.schema_url.to_string_strict() } {
        Ok(schema_url) if !schema_url.is_empty() => builder = builder.with_schema_url(schema_url),
        Ok(_) => {}
        Err(err) => {
            fail_abi(err);
            return std::ptr::null_mut();
        }
    }
    let attributes = match unsafe {
        crate::vtable::collect_unique_key_values(scope.attributes, scope.attribute_count)
    } {
        Ok(attributes) => attributes,
        Err(_) => return std::ptr::null_mut(),
    };
    if !attributes.is_empty() {
        builder = builder.with_attributes(attributes);
    }
    Box::into_raw(Box::new(provider.meter_with_scope(builder.build()))) as *mut c_void
}

extern "C" fn provider_get_meter(
    ctx: *mut c_void,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut c_void {
    guard_ptr(|| {
        build_meter(
            ctx,
            &OtelMetricScopeConfig {
                name,
                version,
                schema_url,
                attributes: std::ptr::null(),
                attribute_count: 0,
            },
        )
    })
}

extern "C" fn provider_get_meter_with_scope(
    ctx: *mut c_void,
    scope: *const OtelMetricScopeConfig,
) -> *mut c_void {
    guard_ptr(|| {
        if scope.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "metric scope config must not be NULL",
            );
            return std::ptr::null_mut();
        }
        build_meter(ctx, unsafe { &*scope })
    })
}

extern "C" fn provider_retain(ctx: *mut c_void) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() {
            return std::ptr::null_mut();
        }
        let provider = unsafe { &*(ctx as *const SdkMeterProvider) };
        Box::into_raw(Box::new(provider.clone())) as *mut c_void
    })
}

extern "C" fn provider_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            drop(unsafe { Box::from_raw(ctx as *mut SdkMeterProvider) });
        }
    });
}

fn configure_sync_builder<T>(
    builder: opentelemetry::metrics::InstrumentBuilder<'_, T>,
    description: String,
    unit: String,
) -> opentelemetry::metrics::InstrumentBuilder<'_, T> {
    let builder = if description.is_empty() {
        builder
    } else {
        builder.with_description(description)
    };
    if unit.is_empty() {
        builder
    } else {
        builder.with_unit(unit)
    }
}

fn configure_histogram_builder<T>(
    builder: opentelemetry::metrics::HistogramBuilder<'_, T>,
    description: String,
    unit: String,
    boundaries: Option<Vec<f64>>,
) -> opentelemetry::metrics::HistogramBuilder<'_, T> {
    let builder = if description.is_empty() {
        builder
    } else {
        builder.with_description(description)
    };
    let builder = if unit.is_empty() {
        builder
    } else {
        builder.with_unit(unit)
    };
    match boundaries {
        Some(boundaries) => builder.with_boundaries(boundaries),
        None => builder,
    }
}

fn configure_async_builder<I, M>(
    builder: opentelemetry::metrics::AsyncInstrumentBuilder<'_, I, M>,
    description: String,
    unit: String,
) -> opentelemetry::metrics::AsyncInstrumentBuilder<'_, I, M> {
    let builder = if description.is_empty() {
        builder
    } else {
        builder.with_description(description)
    };
    if unit.is_empty() {
        builder
    } else {
        builder.with_unit(unit)
    }
}

fn callback_lease(
    config: &OtelMetricInstrumentConfig,
) -> Result<Option<CallbackLease>, OtelStatus> {
    if config.callback_state.is_null() && config.callback_state_free.is_none() {
        return Ok(None);
    }
    let release = config.callback_state_free.ok_or_else(|| {
        fail(
            OtelStatus::InvalidArgument,
            "observable callback state release function is NULL",
        )
    })?;
    if config.callback_state.is_null() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "observable callback state is NULL",
        ));
    }
    Ok(Some(CallbackLease {
        state: config.callback_state,
        release,
    }))
}

fn observable_parts(
    config: &OtelMetricInstrumentConfig,
    lease: &mut Option<CallbackLease>,
) -> Result<
    (
        extern "C" fn(observer_ctx: *mut c_void, state: *mut c_void),
        CallbackLease,
    ),
    OtelStatus,
> {
    let callback = config
        .callback
        .ok_or_else(|| fail(OtelStatus::InvalidArgument, "observable callback is NULL"))?;
    let lease = lease.take().ok_or_else(|| {
        fail(
            OtelStatus::InvalidArgument,
            "observable callback state is missing",
        )
    })?;
    Ok((callback, lease))
}

#[cfg(test)]
static PANIC_AFTER_CALLBACK_TRANSFER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

extern "C" fn meter_create_instrument(
    meter_ctx: *mut c_void,
    config: *const OtelMetricInstrumentConfig,
) -> *mut c_void {
    let mut status = OtelStatus::Ok;
    meter_create_instrument_with_status(meter_ctx, config, &mut status)
}

extern "C" fn meter_create_instrument_with_status(
    meter_ctx: *mut c_void,
    config: *const OtelMetricInstrumentConfig,
    out_status: *mut OtelStatus,
) -> *mut c_void {
    reset_last_status();
    let result = catch_unwind(AssertUnwindSafe(|| {
        if config.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "metric instrument creation received a NULL config",
            );
            return std::ptr::null_mut();
        }
        let config = unsafe { &*config };
        // The Metrics ABI transfers callback-state ownership on entry whenever a release
        // function is present. Keeping the lease in this outer scope guarantees one release
        // on every validation error or caught panic; successful observable builders move it
        // into the SDK callback closure.
        let mut callback_lease = match callback_lease(config) {
            Ok(lease) => lease,
            Err(_) => return std::ptr::null_mut(),
        };
        if meter_ctx.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "metric instrument creation received a NULL meter context",
            );
            return std::ptr::null_mut();
        }
        let meter = unsafe { &*(meter_ctx as *const Meter) };
        let kind = match OtelMetricInstrumentKind::from_u32(config.kind) {
            Some(kind) => kind,
            None => {
                fail(
                    OtelStatus::InvalidArgument,
                    "unknown metric instrument kind",
                );
                return std::ptr::null_mut();
            }
        };
        let number = match OtelMetricNumberKind::from_u32(config.number) {
            Some(number) => number,
            None => {
                fail(OtelStatus::InvalidArgument, "unknown metric number kind");
                return std::ptr::null_mut();
            }
        };
        let name = match unsafe { config.name.to_string_strict() } {
            Ok(name) => name,
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        };
        #[cfg(test)]
        if name == "panic_observable"
            && PANIC_AFTER_CALLBACK_TRANSFER.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("test panic after callback-state ownership transfer");
        }
        let description = match unsafe { config.description.to_string_strict() } {
            Ok(description) => description,
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        };
        let unit = match unsafe { config.unit.to_string_strict() } {
            Ok(unit) => unit,
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        };
        let boundaries = if config.boundary_count == 0 {
            None
        } else {
            if config.boundaries.is_null() {
                fail(
                    OtelStatus::InvalidArgument,
                    "histogram boundaries are NULL with non-zero count",
                );
                return std::ptr::null_mut();
            }
            let slice =
                unsafe { std::slice::from_raw_parts(config.boundaries, config.boundary_count) };
            Some(slice.to_vec())
        };

        let instrument = match (kind, number) {
            (OtelMetricInstrumentKind::Counter, OtelMetricNumberKind::U64) => {
                SdkMetricInstrument::CounterU64(
                    configure_sync_builder(meter.u64_counter(name), description, unit).build(),
                )
            }
            (OtelMetricInstrumentKind::Counter, OtelMetricNumberKind::F64) => {
                SdkMetricInstrument::CounterF64(
                    configure_sync_builder(meter.f64_counter(name), description, unit).build(),
                )
            }
            (OtelMetricInstrumentKind::UpDownCounter, OtelMetricNumberKind::I64) => {
                SdkMetricInstrument::UpDownCounterI64(
                    configure_sync_builder(meter.i64_up_down_counter(name), description, unit)
                        .build(),
                )
            }
            (OtelMetricInstrumentKind::UpDownCounter, OtelMetricNumberKind::F64) => {
                SdkMetricInstrument::UpDownCounterF64(
                    configure_sync_builder(meter.f64_up_down_counter(name), description, unit)
                        .build(),
                )
            }
            (OtelMetricInstrumentKind::Gauge, OtelMetricNumberKind::U64) => {
                SdkMetricInstrument::GaugeU64(
                    configure_sync_builder(meter.u64_gauge(name), description, unit).build(),
                )
            }
            (OtelMetricInstrumentKind::Gauge, OtelMetricNumberKind::I64) => {
                SdkMetricInstrument::GaugeI64(
                    configure_sync_builder(meter.i64_gauge(name), description, unit).build(),
                )
            }
            (OtelMetricInstrumentKind::Gauge, OtelMetricNumberKind::F64) => {
                SdkMetricInstrument::GaugeF64(
                    configure_sync_builder(meter.f64_gauge(name), description, unit).build(),
                )
            }
            (OtelMetricInstrumentKind::Histogram, OtelMetricNumberKind::U64) => {
                SdkMetricInstrument::HistogramU64(
                    configure_histogram_builder(
                        meter.u64_histogram(name),
                        description,
                        unit,
                        boundaries,
                    )
                    .build(),
                )
            }
            (OtelMetricInstrumentKind::Histogram, OtelMetricNumberKind::F64) => {
                SdkMetricInstrument::HistogramF64(
                    configure_histogram_builder(
                        meter.f64_histogram(name),
                        description,
                        unit,
                        boundaries,
                    )
                    .build(),
                )
            }
            (OtelMetricInstrumentKind::ObservableCounter, OtelMetricNumberKind::U64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument =
                    configure_async_builder(meter.u64_observable_counter(name), description, unit)
                        .with_callback(move |observer| {
                            let _keep_alive = &lease;
                            let mut ctx = ObserverCtxU64 { observer };
                            callback((&mut ctx as *mut ObserverCtxU64<'_>).cast(), lease.state);
                        })
                        .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableCounter, OtelMetricNumberKind::F64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument =
                    configure_async_builder(meter.f64_observable_counter(name), description, unit)
                        .with_callback(move |observer| {
                            let _keep_alive = &lease;
                            let mut ctx = ObserverCtxF64 { observer };
                            callback((&mut ctx as *mut ObserverCtxF64<'_>).cast(), lease.state);
                        })
                        .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableUpDownCounter, OtelMetricNumberKind::I64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument = configure_async_builder(
                    meter.i64_observable_up_down_counter(name),
                    description,
                    unit,
                )
                .with_callback(move |observer| {
                    let _keep_alive = &lease;
                    let mut ctx = ObserverCtxI64 { observer };
                    callback((&mut ctx as *mut ObserverCtxI64<'_>).cast(), lease.state);
                })
                .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableUpDownCounter, OtelMetricNumberKind::F64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument = configure_async_builder(
                    meter.f64_observable_up_down_counter(name),
                    description,
                    unit,
                )
                .with_callback(move |observer| {
                    let _keep_alive = &lease;
                    let mut ctx = ObserverCtxF64 { observer };
                    callback((&mut ctx as *mut ObserverCtxF64<'_>).cast(), lease.state);
                })
                .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableGauge, OtelMetricNumberKind::U64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument =
                    configure_async_builder(meter.u64_observable_gauge(name), description, unit)
                        .with_callback(move |observer| {
                            let _keep_alive = &lease;
                            let mut ctx = ObserverCtxU64 { observer };
                            callback((&mut ctx as *mut ObserverCtxU64<'_>).cast(), lease.state);
                        })
                        .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableGauge, OtelMetricNumberKind::I64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument =
                    configure_async_builder(meter.i64_observable_gauge(name), description, unit)
                        .with_callback(move |observer| {
                            let _keep_alive = &lease;
                            let mut ctx = ObserverCtxI64 { observer };
                            callback((&mut ctx as *mut ObserverCtxI64<'_>).cast(), lease.state);
                        })
                        .build();
                SdkMetricInstrument::Observable
            }
            (OtelMetricInstrumentKind::ObservableGauge, OtelMetricNumberKind::F64) => {
                let (callback, lease) = match observable_parts(config, &mut callback_lease) {
                    Ok(parts) => parts,
                    Err(_) => return std::ptr::null_mut(),
                };
                let _instrument =
                    configure_async_builder(meter.f64_observable_gauge(name), description, unit)
                        .with_callback(move |observer| {
                            let _keep_alive = &lease;
                            let mut ctx = ObserverCtxF64 { observer };
                            callback((&mut ctx as *mut ObserverCtxF64<'_>).cast(), lease.state);
                        })
                        .build();
                SdkMetricInstrument::Observable
            }
            _ => {
                fail(
                    OtelStatus::InvalidConfig,
                    "unsupported metric instrument kind and number combination",
                );
                return std::ptr::null_mut();
            }
        };
        Box::into_raw(Box::new(instrument)) as *mut c_void
    }));
    let ctx = match result {
        Ok(ctx) => ctx,
        Err(_) => {
            fail(
                OtelStatus::InternalError,
                "metric instrument creation panicked",
            );
            std::ptr::null_mut()
        }
    };
    if !out_status.is_null() {
        unsafe {
            *out_status = if ctx.is_null() {
                last_status_or(OtelStatus::InvalidConfig)
            } else {
                OtelStatus::Ok
            };
        }
    }
    ctx
}

extern "C" fn meter_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            drop(unsafe { Box::from_raw(ctx as *mut Meter) });
        }
    });
}

macro_rules! record {
    ($name:ident, $value:ty, $($variant:ident => $method:ident),+ $(,)?) => {
        extern "C" fn $name(
            ctx: *mut c_void,
            value: $value,
            attributes: *const opentelemetry_c_abi::OtelKeyValue,
            attribute_count: usize,
        ) -> OtelStatus {
            guard_status(|| {
                if ctx.is_null() {
                    return fail(OtelStatus::InvalidArgument, "metric instrument context is NULL");
                }
                let attributes = match unsafe {
                    crate::vtable::collect_key_values(attributes, attribute_count)
                } {
                    Ok(attributes) => attributes,
                    Err(status) => return status,
                };
                let instrument = unsafe { &*(ctx as *const SdkMetricInstrument) };
                match instrument {
                    $(SdkMetricInstrument::$variant(instrument) => instrument.$method(value, &attributes),)+
                    _ => return fail(
                        OtelStatus::InvalidArgument,
                        "metric operation does not match the instrument numeric type",
                    ),
                }
                OtelStatus::Ok
            })
        }
    };
}

record!(instrument_record_u64, u64,
    CounterU64 => add,
    GaugeU64 => record,
    HistogramU64 => record,
);
record!(instrument_record_i64, i64,
    UpDownCounterI64 => add,
    GaugeI64 => record,
);
record!(instrument_record_f64, f64,
    CounterF64 => add,
    UpDownCounterF64 => add,
    GaugeF64 => record,
    HistogramF64 => record,
);

extern "C" fn observer_observe_u64(
    ctx: *mut c_void,
    value: u64,
    attributes: *const opentelemetry_c_abi::OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(OtelStatus::InvalidArgument, "observer context is NULL");
        }
        let attributes =
            match unsafe { crate::vtable::collect_key_values(attributes, attribute_count) } {
                Ok(attributes) => attributes,
                Err(status) => return status,
            };
        let ctx = unsafe { &*(ctx as *const ObserverCtxU64<'_>) };
        ctx.observer.observe(value, &attributes);
        OtelStatus::Ok
    })
}

extern "C" fn observer_observe_i64(
    ctx: *mut c_void,
    value: i64,
    attributes: *const opentelemetry_c_abi::OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(OtelStatus::InvalidArgument, "observer context is NULL");
        }
        let attributes =
            match unsafe { crate::vtable::collect_key_values(attributes, attribute_count) } {
                Ok(attributes) => attributes,
                Err(status) => return status,
            };
        let ctx = unsafe { &*(ctx as *const ObserverCtxI64<'_>) };
        ctx.observer.observe(value, &attributes);
        OtelStatus::Ok
    })
}

extern "C" fn observer_observe_f64(
    ctx: *mut c_void,
    value: f64,
    attributes: *const opentelemetry_c_abi::OtelKeyValue,
    attribute_count: usize,
) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(OtelStatus::InvalidArgument, "observer context is NULL");
        }
        let attributes =
            match unsafe { crate::vtable::collect_key_values(attributes, attribute_count) } {
                Ok(attributes) => attributes,
                Err(status) => return status,
            };
        let ctx = unsafe { &*(ctx as *const ObserverCtxF64<'_>) };
        ctx.observer.observe(value, &attributes);
        OtelStatus::Ok
    })
}

extern "C" fn instrument_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            drop(unsafe { Box::from_raw(ctx as *mut SdkMetricInstrument) });
        }
    });
}

extern "C" fn instrument_bind(
    ctx: *mut c_void,
    attributes: *const opentelemetry_c_abi::OtelKeyValue,
    attribute_count: usize,
    out_status: *mut OtelStatus,
) -> *mut c_void {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if out_status.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "bound instrument status pointer is NULL",
            );
            return std::ptr::null_mut();
        }
        unsafe { *out_status = OtelStatus::Ok };
        if ctx.is_null() {
            unsafe { *out_status = OtelStatus::InvalidArgument };
            fail(
                OtelStatus::InvalidArgument,
                "metric instrument context is NULL",
            );
            return std::ptr::null_mut();
        }
        let attributes =
            match unsafe { crate::vtable::collect_key_values(attributes, attribute_count) } {
                Ok(attributes) => attributes,
                Err(status) => {
                    unsafe { *out_status = status };
                    return std::ptr::null_mut();
                }
            };
        let instrument = unsafe { &*(ctx as *const SdkMetricInstrument) };
        let bound = match instrument {
            SdkMetricInstrument::CounterU64(instrument) => {
                SdkBoundMetricInstrument::CounterU64(instrument.bind(&attributes))
            }
            SdkMetricInstrument::CounterF64(instrument) => {
                SdkBoundMetricInstrument::CounterF64(instrument.bind(&attributes))
            }
            SdkMetricInstrument::HistogramU64(instrument) => {
                SdkBoundMetricInstrument::HistogramU64(instrument.bind(&attributes))
            }
            SdkMetricInstrument::HistogramF64(instrument) => {
                SdkBoundMetricInstrument::HistogramF64(instrument.bind(&attributes))
            }
            _ => {
                unsafe { *out_status = OtelStatus::InvalidArgument };
                fail(
                    OtelStatus::InvalidArgument,
                    "instrument kind does not support bound instruments",
                );
                return std::ptr::null_mut();
            }
        };
        Box::into_raw(Box::new(bound)) as *mut c_void
    }));
    match result {
        Ok(ctx) => ctx,
        Err(_) => {
            if !out_status.is_null() {
                unsafe { *out_status = OtelStatus::InternalError };
            }
            fail(
                OtelStatus::InternalError,
                "panic while binding metric instrument",
            );
            std::ptr::null_mut()
        }
    }
}

extern "C" fn bound_instrument_record_u64(ctx: *mut c_void, value: u64) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "bound metric instrument context is NULL",
            );
        }
        match unsafe { &*(ctx as *const SdkBoundMetricInstrument) } {
            SdkBoundMetricInstrument::CounterU64(instrument) => instrument.add(value),
            SdkBoundMetricInstrument::HistogramU64(instrument) => instrument.record(value),
            _ => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "bound metric operation does not match the instrument numeric type",
                )
            }
        }
        OtelStatus::Ok
    })
}

extern "C" fn bound_instrument_record_f64(ctx: *mut c_void, value: f64) -> OtelStatus {
    guard_status(|| {
        if ctx.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "bound metric instrument context is NULL",
            );
        }
        match unsafe { &*(ctx as *const SdkBoundMetricInstrument) } {
            SdkBoundMetricInstrument::CounterF64(instrument) => instrument.add(value),
            SdkBoundMetricInstrument::HistogramF64(instrument) => instrument.record(value),
            _ => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "bound metric operation does not match the instrument numeric type",
                )
            }
        }
        OtelStatus::Ok
    })
}

extern "C" fn bound_instrument_free(ctx: *mut c_void) {
    guard_unit(|| {
        if !ctx.is_null() {
            drop(unsafe { Box::from_raw(ctx as *mut SdkBoundMetricInstrument) });
        }
    });
}

pub(crate) static SDK_METRICS_VTABLE: OtelMetricsVtable = OtelMetricsVtable {
    abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
    struct_size: std::mem::size_of::<OtelMetricsVtable>(),
    provider_get_meter,
    provider_retain,
    provider_free,
    meter_create_instrument,
    meter_free,
    instrument_record_u64,
    instrument_record_i64,
    instrument_record_f64,
    observer_observe_u64,
    observer_observe_i64,
    observer_observe_f64,
    instrument_free,
    provider_get_meter_with_scope,
    meter_create_instrument_with_status,
    instrument_bind,
    bound_instrument_record_u64,
    bound_instrument_record_f64,
    bound_instrument_free,
};

pub(crate) fn vtable_ptr() -> *const OtelMetricsVtable {
    &SDK_METRICS_VTABLE
}

pub(crate) fn provider_ctx(provider: SdkMeterProvider) -> *mut c_void {
    Box::into_raw(Box::new(provider)) as *mut c_void
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_char;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use opentelemetry::KeyValue;
    use opentelemetry_c_abi::{OtelAttributeType, OtelAttributeValue, OtelKeyValue};
    use opentelemetry_c_api as api;
    use opentelemetry_sdk::metrics::data::{
        AggregatedMetrics, Metric, MetricData, ResourceMetrics,
    };
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader};

    fn sv(value: &'static str) -> OtelStringView {
        OtelStringView {
            ptr: value.as_ptr().cast::<c_char>(),
            len: value.len(),
        }
    }

    fn config(
        kind: OtelMetricInstrumentKind,
        number: OtelMetricNumberKind,
        name: &'static str,
    ) -> OtelMetricInstrumentConfig {
        OtelMetricInstrumentConfig {
            kind: kind as u32,
            number: number as u32,
            name: sv(name),
            description: OtelStringView::empty(),
            unit: OtelStringView::empty(),
            boundaries: std::ptr::null(),
            boundary_count: 0,
            callback: None,
            callback_state: std::ptr::null_mut(),
            callback_state_free: None,
        }
    }

    fn metric<'a>(metrics: &'a [ResourceMetrics], name: &str) -> &'a Metric {
        metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .find(|metric| metric.name() == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
    }

    fn assert_attributes<'a>(actual: impl Iterator<Item = &'a KeyValue>) {
        let actual: Vec<_> = actual.cloned().collect();
        assert!(actual.contains(&KeyValue::new("route", "/items")));
        assert!(actual.contains(&KeyValue::new("cached", true)));
        assert!(actual.contains(&KeyValue::new("status", 201_i64)));
        assert!(actual.contains(&KeyValue::new("ratio", 0.75_f64)));
    }

    #[test]
    fn synchronous_family_records_through_rust_sdk() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx_raw = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx_raw,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        assert!(!meter.is_null());

        let cases = [
            (
                OtelMetricInstrumentKind::Counter,
                OtelMetricNumberKind::U64,
                "counter_u64",
            ),
            (
                OtelMetricInstrumentKind::Counter,
                OtelMetricNumberKind::F64,
                "counter_f64",
            ),
            (
                OtelMetricInstrumentKind::UpDownCounter,
                OtelMetricNumberKind::I64,
                "up_down_i64",
            ),
            (
                OtelMetricInstrumentKind::UpDownCounter,
                OtelMetricNumberKind::F64,
                "up_down_f64",
            ),
            (
                OtelMetricInstrumentKind::Gauge,
                OtelMetricNumberKind::U64,
                "gauge_u64",
            ),
            (
                OtelMetricInstrumentKind::Gauge,
                OtelMetricNumberKind::I64,
                "gauge_i64",
            ),
            (
                OtelMetricInstrumentKind::Gauge,
                OtelMetricNumberKind::F64,
                "gauge_f64",
            ),
            (
                OtelMetricInstrumentKind::Histogram,
                OtelMetricNumberKind::U64,
                "histogram_u64",
            ),
            (
                OtelMetricInstrumentKind::Histogram,
                OtelMetricNumberKind::F64,
                "histogram_f64",
            ),
        ];
        let mut instruments = Vec::new();
        for (kind, number, name) in cases {
            let config = config(kind, number, name);
            let instrument = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &config);
            assert!(!instrument.is_null(), "{name}");
            let status = match number {
                OtelMetricNumberKind::U64 => {
                    (SDK_METRICS_VTABLE.instrument_record_u64)(instrument, 3, std::ptr::null(), 0)
                }
                OtelMetricNumberKind::I64 => {
                    (SDK_METRICS_VTABLE.instrument_record_i64)(instrument, -3, std::ptr::null(), 0)
                }
                OtelMetricNumberKind::F64 => {
                    (SDK_METRICS_VTABLE.instrument_record_f64)(instrument, 3.5, std::ptr::null(), 0)
                }
            };
            assert_eq!(status, OtelStatus::Ok);
            instruments.push(instrument);
        }

        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        let names: Vec<_> = metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .map(|metric| metric.name())
            .collect();
        for (_, _, name) in cases {
            assert!(names.contains(&name), "missing {name}: {names:?}");
        }

        for instrument in instruments {
            (SDK_METRICS_VTABLE.instrument_free)(instrument);
        }
        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx_raw);
    }

    #[test]
    fn bound_counter_and_histogram_record_prebound_attributes() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx_raw = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx_raw,
            sv("bound_scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        let attributes = [OtelKeyValue {
            key: sv("route"),
            value_type: OtelAttributeType::String as u32,
            value: OtelAttributeValue {
                string_value: sv("/bound"),
            },
        }];

        let counter_config = config(
            OtelMetricInstrumentKind::Counter,
            OtelMetricNumberKind::U64,
            "bound_counter",
        );
        let counter = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &counter_config);
        let mut status = OtelStatus::Ok;
        let bound_counter = (SDK_METRICS_VTABLE.instrument_bind)(
            counter,
            attributes.as_ptr(),
            attributes.len(),
            &mut status,
        );
        assert_eq!(status, OtelStatus::Ok);
        assert!(!bound_counter.is_null());
        // A bound upstream instrument owns everything needed to outlive this wrapper.
        (SDK_METRICS_VTABLE.instrument_free)(counter);
        assert_eq!(
            (SDK_METRICS_VTABLE.bound_instrument_record_u64)(bound_counter, 3),
            OtelStatus::Ok
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.bound_instrument_record_u64)(bound_counter, 4),
            OtelStatus::Ok
        );

        let histogram_config = config(
            OtelMetricInstrumentKind::Histogram,
            OtelMetricNumberKind::F64,
            "bound_histogram",
        );
        let histogram = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &histogram_config);
        let bound_histogram = (SDK_METRICS_VTABLE.instrument_bind)(
            histogram,
            attributes.as_ptr(),
            attributes.len(),
            &mut status,
        );
        assert_eq!(status, OtelStatus::Ok);
        assert!(!bound_histogram.is_null());
        (SDK_METRICS_VTABLE.instrument_free)(histogram);
        assert_eq!(
            (SDK_METRICS_VTABLE.bound_instrument_record_f64)(bound_histogram, 2.5),
            OtelStatus::Ok
        );

        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        match metric(&metrics, "bound_counter").data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                let point = sum.data_points().next().unwrap();
                assert_eq!(point.value(), 7);
                assert_eq!(
                    point.attributes().cloned().collect::<Vec<_>>(),
                    vec![KeyValue::new("route", "/bound")]
                );
            }
            other => panic!("unexpected counter aggregation: {other:?}"),
        }
        match metric(&metrics, "bound_histogram").data() {
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                let point = histogram.data_points().next().unwrap();
                assert_eq!(point.count(), 1);
                assert_eq!(point.sum(), 2.5);
                assert_eq!(
                    point.attributes().cloned().collect::<Vec<_>>(),
                    vec![KeyValue::new("route", "/bound")]
                );
            }
            other => panic!("unexpected histogram aggregation: {other:?}"),
        }

        (SDK_METRICS_VTABLE.bound_instrument_free)(bound_counter);
        (SDK_METRICS_VTABLE.bound_instrument_free)(bound_histogram);
        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx_raw);
    }

    #[test]
    fn synchronous_values_metadata_attributes_and_scope_are_exported() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx_raw = provider_ctx(provider.clone());
        let scope_attributes = [
            OtelKeyValue {
                key: sv("scope.component"),
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: sv("checkout"),
                },
            },
            OtelKeyValue {
                key: sv("scope.stable"),
                value_type: OtelAttributeType::Bool as u32,
                value: OtelAttributeValue { bool_value: 1 },
            },
        ];
        let scope_config = OtelMetricScopeConfig {
            name: sv("semantic_scope"),
            version: sv("2.1.0"),
            schema_url: sv("https://example.test/schema"),
            attributes: scope_attributes.as_ptr(),
            attribute_count: scope_attributes.len(),
        };
        let meter =
            (SDK_METRICS_VTABLE.provider_get_meter_with_scope)(provider_ctx_raw, &scope_config);
        let attributes = [
            OtelKeyValue {
                key: sv("route"),
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: sv("/items"),
                },
            },
            OtelKeyValue {
                key: sv("cached"),
                value_type: OtelAttributeType::Bool as u32,
                value: OtelAttributeValue { bool_value: 1 },
            },
            OtelKeyValue {
                key: sv("status"),
                value_type: OtelAttributeType::Int64 as u32,
                value: OtelAttributeValue { int64_value: 201 },
            },
            OtelKeyValue {
                key: sv("ratio"),
                value_type: OtelAttributeType::Double as u32,
                value: OtelAttributeValue { double_value: 0.75 },
            },
        ];
        let boundaries = [1.0, 5.0, 10.0];
        let mut handles = Vec::new();
        let mut create = |kind, number, name, histogram: bool| {
            let mut config = config(kind, number, name);
            config.description = sv("semantic description");
            config.unit = sv("widgets");
            if histogram {
                config.boundaries = boundaries.as_ptr();
                config.boundary_count = boundaries.len();
            }
            let handle = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &config);
            assert!(!handle.is_null(), "{name}");
            handles.push(handle);
            handle
        };

        let counter_u64 = create(
            OtelMetricInstrumentKind::Counter,
            OtelMetricNumberKind::U64,
            "value_counter_u64",
            false,
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_u64)(
                counter_u64,
                3,
                attributes.as_ptr(),
                attributes.len(),
            ),
            OtelStatus::Ok
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_u64)(
                counter_u64,
                4,
                attributes.as_ptr(),
                attributes.len(),
            ),
            OtelStatus::Ok
        );

        let counter_f64 = create(
            OtelMetricInstrumentKind::Counter,
            OtelMetricNumberKind::F64,
            "value_counter_f64",
            false,
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_f64)(counter_f64, 1.25, std::ptr::null(), 0,),
            OtelStatus::Ok
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_f64)(counter_f64, 2.5, std::ptr::null(), 0,),
            OtelStatus::Ok
        );

        let up_down_i64 = create(
            OtelMetricInstrumentKind::UpDownCounter,
            OtelMetricNumberKind::I64,
            "value_up_down_i64",
            false,
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_i64)(up_down_i64, 5, std::ptr::null(), 0),
            OtelStatus::Ok
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_i64)(up_down_i64, -8, std::ptr::null(), 0),
            OtelStatus::Ok
        );

        let up_down_f64 = create(
            OtelMetricInstrumentKind::UpDownCounter,
            OtelMetricNumberKind::F64,
            "value_up_down_f64",
            false,
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_f64)(up_down_f64, 4.5, std::ptr::null(), 0,),
            OtelStatus::Ok
        );
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_f64)(up_down_f64, -1.25, std::ptr::null(), 0,),
            OtelStatus::Ok
        );

        let gauge_u64 = create(
            OtelMetricInstrumentKind::Gauge,
            OtelMetricNumberKind::U64,
            "value_gauge_u64",
            false,
        );
        (SDK_METRICS_VTABLE.instrument_record_u64)(gauge_u64, 10, std::ptr::null(), 0);
        (SDK_METRICS_VTABLE.instrument_record_u64)(gauge_u64, 3, std::ptr::null(), 0);
        let gauge_i64 = create(
            OtelMetricInstrumentKind::Gauge,
            OtelMetricNumberKind::I64,
            "value_gauge_i64",
            false,
        );
        (SDK_METRICS_VTABLE.instrument_record_i64)(gauge_i64, 4, std::ptr::null(), 0);
        (SDK_METRICS_VTABLE.instrument_record_i64)(gauge_i64, -2, std::ptr::null(), 0);
        let gauge_f64 = create(
            OtelMetricInstrumentKind::Gauge,
            OtelMetricNumberKind::F64,
            "value_gauge_f64",
            false,
        );
        (SDK_METRICS_VTABLE.instrument_record_f64)(gauge_f64, 8.0, std::ptr::null(), 0);
        (SDK_METRICS_VTABLE.instrument_record_f64)(gauge_f64, 2.25, std::ptr::null(), 0);

        let histogram_u64 = create(
            OtelMetricInstrumentKind::Histogram,
            OtelMetricNumberKind::U64,
            "value_histogram_u64",
            true,
        );
        for value in [0, 3, 7, 12] {
            (SDK_METRICS_VTABLE.instrument_record_u64)(histogram_u64, value, std::ptr::null(), 0);
        }
        let histogram_f64 = create(
            OtelMetricInstrumentKind::Histogram,
            OtelMetricNumberKind::F64,
            "value_histogram_f64",
            true,
        );
        for value in [2.5, 6.5] {
            (SDK_METRICS_VTABLE.instrument_record_f64)(histogram_f64, value, std::ptr::null(), 0);
        }

        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        let scope = metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .find(|scope| scope.scope().name() == "semantic_scope")
            .expect("semantic scope");
        assert_eq!(scope.scope().version(), Some("2.1.0"));
        assert_eq!(
            scope.scope().schema_url(),
            Some("https://example.test/schema")
        );
        assert_eq!(
            scope.scope().attributes().cloned().collect::<Vec<_>>(),
            vec![
                KeyValue::new("scope.component", "checkout"),
                KeyValue::new("scope.stable", true),
            ]
        );
        for metric in scope.metrics() {
            assert_eq!(metric.description(), "semantic description");
            assert_eq!(metric.unit(), "widgets");
        }

        match metric(&metrics, "value_counter_u64").data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                assert!(sum.is_monotonic());
                let point = sum.data_points().next().unwrap();
                assert_eq!(point.value(), 7);
                assert_attributes(point.attributes());
            }
            data => panic!("unexpected counter data: {data:?}"),
        }
        match metric(&metrics, "value_counter_f64").data() {
            AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                assert!(sum.is_monotonic());
                assert_eq!(sum.data_points().next().unwrap().value(), 3.75);
            }
            data => panic!("unexpected counter data: {data:?}"),
        }
        match metric(&metrics, "value_up_down_i64").data() {
            AggregatedMetrics::I64(MetricData::Sum(sum)) => {
                assert!(!sum.is_monotonic());
                assert_eq!(sum.data_points().next().unwrap().value(), -3);
            }
            data => panic!("unexpected up-down data: {data:?}"),
        }
        match metric(&metrics, "value_up_down_f64").data() {
            AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                assert!(!sum.is_monotonic());
                assert_eq!(sum.data_points().next().unwrap().value(), 3.25);
            }
            data => panic!("unexpected up-down data: {data:?}"),
        }
        match metric(&metrics, "value_gauge_u64").data() {
            AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().value(), 3);
            }
            data => panic!("unexpected gauge data: {data:?}"),
        }
        match metric(&metrics, "value_gauge_i64").data() {
            AggregatedMetrics::I64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().value(), -2);
            }
            data => panic!("unexpected gauge data: {data:?}"),
        }
        match metric(&metrics, "value_gauge_f64").data() {
            AggregatedMetrics::F64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().value(), 2.25);
            }
            data => panic!("unexpected gauge data: {data:?}"),
        }
        match metric(&metrics, "value_histogram_u64").data() {
            AggregatedMetrics::U64(MetricData::Histogram(histogram)) => {
                let point = histogram.data_points().next().unwrap();
                assert_eq!(point.count(), 4);
                assert_eq!(point.sum(), 22);
                assert_eq!(point.min(), Some(0));
                assert_eq!(point.max(), Some(12));
                assert_eq!(point.bounds().collect::<Vec<_>>(), boundaries);
                assert_eq!(point.bucket_counts().collect::<Vec<_>>(), [1, 1, 1, 1]);
            }
            data => panic!("unexpected histogram data: {data:?}"),
        }
        match metric(&metrics, "value_histogram_f64").data() {
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                let point = histogram.data_points().next().unwrap();
                assert_eq!(point.count(), 2);
                assert_eq!(point.sum(), 9.0);
                assert_eq!(point.min(), Some(2.5));
                assert_eq!(point.max(), Some(6.5));
                assert_eq!(point.bounds().collect::<Vec<_>>(), boundaries);
                assert_eq!(point.bucket_counts().collect::<Vec<_>>(), [0, 1, 1, 0]);
            }
            data => panic!("unexpected histogram data: {data:?}"),
        }

        for handle in handles {
            (SDK_METRICS_VTABLE.instrument_free)(handle);
        }
        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx_raw);
    }

    extern "C" fn observable_callback(observer_ctx: *mut c_void, state: *mut c_void) {
        let count = unsafe { &*(state as *const AtomicUsize) };
        count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            (SDK_METRICS_VTABLE.observer_observe_u64)(observer_ctx, 9, std::ptr::null(), 0),
            OtelStatus::Ok
        );
    }

    extern "C" fn observable_callback_i64(observer_ctx: *mut c_void, state: *mut c_void) {
        let count = unsafe { &*(state as *const AtomicUsize) };
        count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            (SDK_METRICS_VTABLE.observer_observe_i64)(observer_ctx, -9, std::ptr::null(), 0),
            OtelStatus::Ok
        );
    }

    extern "C" fn observable_callback_f64(observer_ctx: *mut c_void, state: *mut c_void) {
        let count = unsafe { &*(state as *const AtomicUsize) };
        count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            (SDK_METRICS_VTABLE.observer_observe_f64)(observer_ctx, 9.5, std::ptr::null(), 0),
            OtelStatus::Ok
        );
    }

    extern "C" fn release_count(state: *mut c_void) {
        drop(unsafe { Arc::from_raw(state as *const AtomicUsize) });
    }

    fn callback_config(
        count: &Arc<AtomicUsize>,
        kind: OtelMetricInstrumentKind,
        number: OtelMetricNumberKind,
        name: &'static str,
    ) -> OtelMetricInstrumentConfig {
        let mut config = config(kind, number, name);
        config.callback = Some(match number {
            OtelMetricNumberKind::U64 => observable_callback,
            OtelMetricNumberKind::I64 => observable_callback_i64,
            OtelMetricNumberKind::F64 => observable_callback_f64,
        });
        config.callback_state = Arc::into_raw(Arc::clone(count)) as *mut c_void;
        config.callback_state_free = Some(release_count);
        config
    }

    #[test]
    fn observable_family_dispatches_each_number_kind() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx,
            sv("observable_scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        let cases = [
            (
                OtelMetricInstrumentKind::ObservableCounter,
                OtelMetricNumberKind::U64,
                "observable_counter_u64",
            ),
            (
                OtelMetricInstrumentKind::ObservableCounter,
                OtelMetricNumberKind::F64,
                "observable_counter_f64",
            ),
            (
                OtelMetricInstrumentKind::ObservableUpDownCounter,
                OtelMetricNumberKind::I64,
                "observable_up_down_i64",
            ),
            (
                OtelMetricInstrumentKind::ObservableUpDownCounter,
                OtelMetricNumberKind::F64,
                "observable_up_down_f64",
            ),
            (
                OtelMetricInstrumentKind::ObservableGauge,
                OtelMetricNumberKind::U64,
                "observable_gauge_u64",
            ),
            (
                OtelMetricInstrumentKind::ObservableGauge,
                OtelMetricNumberKind::I64,
                "observable_gauge_i64",
            ),
            (
                OtelMetricInstrumentKind::ObservableGauge,
                OtelMetricNumberKind::F64,
                "observable_gauge_f64",
            ),
        ];
        let count = Arc::new(AtomicUsize::new(0));
        let mut instruments = Vec::new();
        for (kind, number, name) in cases {
            let config = callback_config(&count, kind, number, name);
            let instrument = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &config);
            assert!(!instrument.is_null(), "{name}");
            instruments.push(instrument);
        }

        provider.force_flush().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), cases.len());
        let metrics = exporter.get_finished_metrics().unwrap();
        for (kind, number, name) in cases {
            match (kind, number, metric(&metrics, name).data()) {
                (
                    OtelMetricInstrumentKind::ObservableCounter,
                    OtelMetricNumberKind::U64,
                    AggregatedMetrics::U64(MetricData::Sum(sum)),
                ) => {
                    assert!(sum.is_monotonic());
                    assert_eq!(sum.data_points().next().unwrap().value(), 9);
                }
                (
                    OtelMetricInstrumentKind::ObservableCounter,
                    OtelMetricNumberKind::F64,
                    AggregatedMetrics::F64(MetricData::Sum(sum)),
                ) => {
                    assert!(sum.is_monotonic());
                    assert_eq!(sum.data_points().next().unwrap().value(), 9.5);
                }
                (
                    OtelMetricInstrumentKind::ObservableUpDownCounter,
                    OtelMetricNumberKind::I64,
                    AggregatedMetrics::I64(MetricData::Sum(sum)),
                ) => {
                    assert!(!sum.is_monotonic());
                    assert_eq!(sum.data_points().next().unwrap().value(), -9);
                }
                (
                    OtelMetricInstrumentKind::ObservableUpDownCounter,
                    OtelMetricNumberKind::F64,
                    AggregatedMetrics::F64(MetricData::Sum(sum)),
                ) => {
                    assert!(!sum.is_monotonic());
                    assert_eq!(sum.data_points().next().unwrap().value(), 9.5);
                }
                (
                    OtelMetricInstrumentKind::ObservableGauge,
                    OtelMetricNumberKind::U64,
                    AggregatedMetrics::U64(MetricData::Gauge(gauge)),
                ) => assert_eq!(gauge.data_points().next().unwrap().value(), 9),
                (
                    OtelMetricInstrumentKind::ObservableGauge,
                    OtelMetricNumberKind::I64,
                    AggregatedMetrics::I64(MetricData::Gauge(gauge)),
                ) => assert_eq!(gauge.data_points().next().unwrap().value(), -9),
                (
                    OtelMetricInstrumentKind::ObservableGauge,
                    OtelMetricNumberKind::F64,
                    AggregatedMetrics::F64(MetricData::Gauge(gauge)),
                ) => assert_eq!(gauge.data_points().next().unwrap().value(), 9.5),
                (_, _, data) => panic!("unexpected observable data for {name}: {data:?}"),
            }
        }

        for instrument in instruments {
            (SDK_METRICS_VTABLE.instrument_free)(instrument);
        }
        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx);
        drop(provider);
        assert_eq!(Arc::strong_count(&count), 1);
    }

    #[test]
    fn multiple_readers_collect_independently_and_invoke_observables() {
        let first_exporter = InMemoryMetricExporter::default();
        let second_exporter = InMemoryMetricExporter::default();
        let first_reader = PeriodicReader::builder(first_exporter.clone()).build();
        let second_reader = PeriodicReader::builder(second_exporter.clone()).build();
        let provider = SdkMeterProvider::builder()
            .with_reader(first_reader)
            .with_reader(second_reader)
            .build();
        let provider_ctx = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx,
            sv("multiple_readers"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let observable_config = callback_config(
            &count,
            OtelMetricInstrumentKind::ObservableGauge,
            OtelMetricNumberKind::U64,
            "reader_observable",
        );
        let observable = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &observable_config);
        assert!(!observable.is_null());
        let counter_config = config(
            OtelMetricInstrumentKind::Counter,
            OtelMetricNumberKind::U64,
            "reader_counter",
        );
        let counter = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &counter_config);
        assert!(!counter.is_null());
        assert_eq!(
            (SDK_METRICS_VTABLE.instrument_record_u64)(counter, 6, std::ptr::null(), 0),
            OtelStatus::Ok
        );

        provider.force_flush().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        for exporter in [&first_exporter, &second_exporter] {
            let metrics = exporter.get_finished_metrics().unwrap();
            match metric(&metrics, "reader_counter").data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    assert_eq!(sum.data_points().next().unwrap().value(), 6);
                }
                data => panic!("unexpected counter data: {data:?}"),
            }
            match metric(&metrics, "reader_observable").data() {
                AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                    assert_eq!(gauge.data_points().next().unwrap().value(), 9);
                }
                data => panic!("unexpected observable data: {data:?}"),
            }
        }

        (SDK_METRICS_VTABLE.instrument_free)(observable);
        (SDK_METRICS_VTABLE.instrument_free)(counter);
        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx);
        drop(provider);
        assert_eq!(Arc::strong_count(&count), 1);
    }

    #[test]
    fn null_config_is_rejected_without_dereference() {
        let mut status = OtelStatus::Ok;
        assert!((SDK_METRICS_VTABLE.meter_create_instrument_with_status)(
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut status,
        )
        .is_null());
        assert_eq!(status, OtelStatus::InvalidArgument);
        assert!((SDK_METRICS_VTABLE.meter_create_instrument)(
            std::ptr::null_mut(),
            std::ptr::null()
        )
        .is_null());
    }

    #[test]
    fn null_meter_releases_transferred_callback_state_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let config = callback_config(
            &count,
            OtelMetricInstrumentKind::ObservableGauge,
            OtelMetricNumberKind::U64,
            "null_meter",
        );
        assert!(
            (SDK_METRICS_VTABLE.meter_create_instrument)(std::ptr::null_mut(), &config).is_null()
        );
        assert_eq!(Arc::strong_count(&count), 1);
    }

    #[test]
    fn validation_failure_releases_transferred_callback_state_once() {
        let provider = SdkMeterProvider::builder().build();
        let provider_ctx = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let mut config = callback_config(
            &count,
            OtelMetricInstrumentKind::ObservableGauge,
            OtelMetricNumberKind::U64,
            "invalid_kind",
        );
        config.kind = u32::MAX;
        assert!((SDK_METRICS_VTABLE.meter_create_instrument)(meter, &config).is_null());
        assert_eq!(Arc::strong_count(&count), 1);
        (SDK_METRICS_VTABLE.meter_free)(meter);
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx);
    }

    #[test]
    fn observable_callback_is_collected_and_state_outlives_handle() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx = provider_ctx(provider.clone());
        let meter = (SDK_METRICS_VTABLE.provider_get_meter)(
            provider_ctx,
            sv("scope"),
            OtelStringView::empty(),
            OtelStringView::empty(),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let mut config = config(
            OtelMetricInstrumentKind::ObservableGauge,
            OtelMetricNumberKind::U64,
            "observable",
        );
        config.callback = Some(observable_callback);
        config.callback_state = Arc::into_raw(Arc::clone(&count)) as *mut c_void;
        config.callback_state_free = Some(release_count);
        let instrument = (SDK_METRICS_VTABLE.meter_create_instrument)(meter, &config);
        assert!(!instrument.is_null());
        (SDK_METRICS_VTABLE.instrument_free)(instrument);

        provider.force_flush().unwrap();
        assert!(count.load(Ordering::SeqCst) >= 1);
        assert!(exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .any(|metric| metric.name() == "observable"));

        (SDK_METRICS_VTABLE.meter_free)(meter);
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx);
        drop(provider);
        assert_eq!(Arc::strong_count(&count), 1);
    }

    struct ApiCallbackState {
        calls: AtomicUsize,
        observer_token: AtomicUsize,
    }

    extern "C" fn api_callback(observer: *mut api::OtelObserverU64, user_data: *mut c_void) {
        let state = unsafe { &*(user_data as *const ApiCallbackState) };
        state
            .observer_token
            .store(observer as usize, Ordering::SeqCst);
        state.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            unsafe { api::otel_observer_u64_observe(observer, 11, std::ptr::null(), 0) },
            OtelStatus::Ok
        );
    }

    extern "C" fn api_state_destroy(user_data: *mut c_void) {
        drop(unsafe { Arc::from_raw(user_data as *const ApiCallbackState) });
    }

    struct MultiObservationState {
        calls: AtomicUsize,
    }

    extern "C" fn multi_observation_callback(
        observer: *mut api::OtelObserverU64,
        user_data: *mut c_void,
    ) {
        let state = unsafe { &*(user_data as *const MultiObservationState) };
        state.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            unsafe { api::otel_observer_u64_observe(std::ptr::null_mut(), 1, std::ptr::null(), 0) },
            OtelStatus::InvalidArgument
        );
        assert_eq!(
            unsafe {
                api::otel_observer_i64_observe(
                    observer.cast::<api::OtelObserverI64>(),
                    -1,
                    std::ptr::null(),
                    0,
                )
            },
            OtelStatus::InvalidArgument
        );
        for (route, value) in [("first", 4), ("second", 7)] {
            let attribute = OtelKeyValue {
                key: sv("route"),
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: sv(route),
                },
            };
            assert_eq!(
                unsafe { api::otel_observer_u64_observe(observer, value, &attribute, 1) },
                OtelStatus::Ok
            );
        }
    }

    extern "C" fn multi_observation_state_destroy(user_data: *mut c_void) {
        drop(unsafe { Arc::from_raw(user_data as *const MultiObservationState) });
    }

    struct PanicUserData {
        destroyed: Arc<AtomicUsize>,
    }

    extern "C" fn panic_test_callback(
        _observer: *mut api::OtelObserverU64,
        _user_data: *mut c_void,
    ) {
    }

    extern "C" fn panic_user_data_destroy(user_data: *mut c_void) {
        let state = unsafe { Box::from_raw(user_data as *mut PanicUserData) };
        state.destroyed.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn sdk_creation_panic_releases_callback_state_but_preserves_user_data_ownership() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let provider_ctx_raw = provider_ctx(provider.clone());
        let destroyed = Arc::new(AtomicUsize::new(0));
        let user_data = Box::into_raw(Box::new(PanicUserData {
            destroyed: Arc::clone(&destroyed),
        }))
        .cast();
        let options = api::OtelInstrumentOptions {
            struct_size: std::mem::size_of::<api::OtelInstrumentOptions>() as u64,
            description: OtelStringView::empty(),
            unit: OtelStringView::empty(),
            boundaries: std::ptr::null(),
            boundary_count: 0,
        };
        let mut registration_id = 0;
        assert_eq!(
            unsafe {
                api::otel_api_register_global_meter_provider_with_token(
                    &SDK_METRICS_VTABLE,
                    provider_ctx(provider.clone()),
                    &mut registration_id,
                )
            },
            OtelStatus::Ok
        );
        let api_provider = api::otel_global_meter_provider();
        let api_meter = unsafe {
            api::otel_meter_provider_get_meter(
                api_provider,
                sv("panic_scope"),
                OtelStringView::empty(),
                OtelStringView::empty(),
            )
        };
        let mut observable = std::ptr::null_mut();

        PANIC_AFTER_CALLBACK_TRANSFER.store(true, Ordering::SeqCst);
        assert_eq!(
            unsafe {
                api::otel_meter_create_u64_observable_gauge(
                    api_meter,
                    sv("panic_observable"),
                    &options,
                    Some(panic_test_callback),
                    user_data,
                    Some(panic_user_data_destroy),
                    &mut observable,
                )
            },
            OtelStatus::InternalError
        );
        assert_eq!(
            crate::api_ffi::test_probe::last_error(),
            "metric instrument creation panicked"
        );
        assert!(observable.is_null());
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        panic_user_data_destroy(user_data);
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);

        unsafe {
            api::otel_meter_destroy(api_meter);
            api::otel_meter_provider_destroy(api_provider);
        }
        assert_eq!(
            api::otel_api_unregister_global_meter_provider(registration_id),
            OtelStatus::Ok
        );
        provider.shutdown().unwrap();
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx_raw);
    }

    #[test]
    fn public_observer_validates_type_and_exports_multiple_observations() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let global_ctx = provider_ctx(provider.clone());
        let mut registration_id = 0;
        assert_eq!(
            unsafe {
                api::otel_api_register_global_meter_provider_with_token(
                    &SDK_METRICS_VTABLE,
                    global_ctx,
                    &mut registration_id,
                )
            },
            OtelStatus::Ok
        );

        let api_provider = api::otel_global_meter_provider();
        let meter = unsafe {
            api::otel_meter_provider_get_meter(
                api_provider,
                sv("multi_observation_scope"),
                OtelStringView::empty(),
                OtelStringView::empty(),
            )
        };
        let state = Arc::new(MultiObservationState {
            calls: AtomicUsize::new(0),
        });
        let mut observable = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                api::otel_meter_create_u64_observable_counter(
                    meter,
                    sv("multi_observation"),
                    std::ptr::null(),
                    Some(multi_observation_callback),
                    Arc::into_raw(Arc::clone(&state)) as *mut c_void,
                    Some(multi_observation_state_destroy),
                    &mut observable,
                )
            },
            OtelStatus::Ok
        );

        provider.force_flush().unwrap();
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        let metrics = exporter.get_finished_metrics().unwrap();
        match metric(&metrics, "multi_observation").data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                assert!(sum.is_monotonic());
                let mut points = sum
                    .data_points()
                    .map(|point| {
                        let route = point
                            .attributes()
                            .find(|attribute| attribute.key.as_str() == "route")
                            .expect("route attribute")
                            .value
                            .as_str()
                            .into_owned();
                        (route, point.value())
                    })
                    .collect::<Vec<_>>();
                points.sort();
                assert_eq!(points, [("first".to_owned(), 4), ("second".to_owned(), 7)]);
            }
            data => panic!("unexpected multi-observation data: {data:?}"),
        }

        unsafe {
            api::otel_observable_counter_u64_destroy(observable);
            api::otel_meter_destroy(meter);
            api::otel_meter_provider_destroy(api_provider);
        }
        assert_eq!(Arc::strong_count(&state), 1);
        assert_eq!(
            api::otel_api_unregister_global_meter_provider(registration_id),
            OtelStatus::Ok
        );
        provider.shutdown().unwrap();
        drop(provider);
    }

    #[test]
    fn public_observer_token_expires_and_destroy_disables_callback() {
        let _global_guard = crate::api_ffi::test_probe::METRICS_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let global_ctx = provider_ctx(provider.clone());
        let mut registration_id = 0;
        assert_eq!(
            unsafe {
                api::otel_api_register_global_meter_provider_with_token(
                    &SDK_METRICS_VTABLE,
                    global_ctx,
                    &mut registration_id,
                )
            },
            OtelStatus::Ok
        );

        let api_provider = api::otel_global_meter_provider();
        let meter = unsafe {
            api::otel_meter_provider_get_meter(
                api_provider,
                sv("public_scope"),
                OtelStringView::empty(),
                OtelStringView::empty(),
            )
        };
        let state = Arc::new(ApiCallbackState {
            calls: AtomicUsize::new(0),
            observer_token: AtomicUsize::new(0),
        });
        let user_data = Arc::into_raw(Arc::clone(&state)) as *mut c_void;
        let options = api::OtelInstrumentOptions {
            struct_size: std::mem::size_of::<api::OtelInstrumentOptions>() as u64,
            description: OtelStringView::empty(),
            unit: OtelStringView::empty(),
            boundaries: std::ptr::null(),
            boundary_count: 0,
        };
        let mut observable = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                api::otel_meter_create_u64_observable_gauge(
                    meter,
                    sv("public_observable"),
                    &options,
                    Some(api_callback),
                    user_data,
                    Some(api_state_destroy),
                    &mut observable,
                )
            },
            OtelStatus::Ok
        );
        provider.force_flush().unwrap();
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);

        let expired = state.observer_token.load(Ordering::SeqCst) as *mut api::OtelObserverU64;
        assert_eq!(
            unsafe { api::otel_observer_u64_observe(expired, 1, std::ptr::null(), 0) },
            OtelStatus::InvalidArgument
        );

        unsafe { api::otel_observable_gauge_u64_destroy(observable) };
        assert_eq!(Arc::strong_count(&state), 1);
        provider.force_flush().unwrap();
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);

        unsafe {
            api::otel_meter_destroy(meter);
            api::otel_meter_provider_destroy(api_provider);
        }
        assert_eq!(
            api::otel_api_unregister_global_meter_provider(registration_id),
            OtelStatus::Ok
        );
        provider.shutdown().unwrap();
        drop(provider);

        let replacement = SdkMeterProvider::default();
        assert_eq!(
            unsafe {
                api::otel_api_register_global_meter_provider(
                    &SDK_METRICS_VTABLE,
                    provider_ctx(replacement),
                )
            },
            OtelStatus::Ok
        );
        assert_eq!(Arc::strong_count(&state), 1);
    }
}
