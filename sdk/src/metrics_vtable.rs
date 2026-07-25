//! SDK implementation of the internal Metrics vtable.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

use opentelemetry::metrics::{
    AsyncInstrument, Counter, Gauge, Histogram, Meter, MeterProvider, UpDownCounter,
};
use opentelemetry::InstrumentationScope;
use opentelemetry_c_abi::{
    OtelMetricInstrumentConfig, OtelMetricInstrumentKind, OtelMetricNumberKind, OtelMetricsVtable,
    OtelStatus, OtelStringView, OTEL_IMPL_ABI_VERSION,
};
use opentelemetry_sdk::metrics::SdkMeterProvider;

use crate::error::{fail, fail_abi};

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

extern "C" fn provider_get_meter(
    ctx: *mut c_void,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut c_void {
    guard_ptr(|| {
        if ctx.is_null() {
            return std::ptr::null_mut();
        }
        let provider = unsafe { &*(ctx as *const SdkMeterProvider) };
        let name = match unsafe { name.to_string_strict() } {
            Ok(name) => name,
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        };
        let mut scope = InstrumentationScope::builder(name);
        match unsafe { version.to_string_strict() } {
            Ok(version) if !version.is_empty() => scope = scope.with_version(version),
            Ok(_) => {}
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        }
        match unsafe { schema_url.to_string_strict() } {
            Ok(schema_url) if !schema_url.is_empty() => scope = scope.with_schema_url(schema_url),
            Ok(_) => {}
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        }
        Box::into_raw(Box::new(provider.meter_with_scope(scope.build()))) as *mut c_void
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
    guard_ptr(|| {
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
    })
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

pub(crate) static SDK_METRICS_VTABLE: OtelMetricsVtable = OtelMetricsVtable {
    abi_version: OTEL_IMPL_ABI_VERSION,
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

    use opentelemetry_c_api as api;
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
        (SDK_METRICS_VTABLE.provider_free)(provider_ctx);
        drop(provider);
        assert_eq!(Arc::strong_count(&count), 1);
    }

    #[test]
    fn null_config_is_rejected_without_dereference() {
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
    fn callback_state_is_released_once_when_sdk_creation_panics() {
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
            OtelStatus::InvalidConfig
        );
        assert!(observable.is_null());
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
