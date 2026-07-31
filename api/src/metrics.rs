//! Public OpenTelemetry Metrics API.

use std::cell::RefCell;
use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use opentelemetry_c_abi::{
    metrics_vtable_supports_bound_instruments, metrics_vtable_supports_creation_status,
    metrics_vtable_supports_scope_config, OtelAttributeType, OtelHandleHeader, OtelKeyValue,
    OtelMetricInstrumentConfig, OtelMetricInstrumentKind, OtelMetricNumberKind,
    OtelMetricScopeConfig, OtelMetricsVtable, OtelStringView, OTEL_HANDLE_KIND_BOUND_COUNTER_F64,
    OTEL_HANDLE_KIND_BOUND_COUNTER_U64, OTEL_HANDLE_KIND_BOUND_HISTOGRAM_F64,
    OTEL_HANDLE_KIND_BOUND_HISTOGRAM_U64, OTEL_HANDLE_KIND_COUNTER_F64,
    OTEL_HANDLE_KIND_COUNTER_U64, OTEL_HANDLE_KIND_GAUGE_F64, OTEL_HANDLE_KIND_GAUGE_I64,
    OTEL_HANDLE_KIND_GAUGE_U64, OTEL_HANDLE_KIND_HISTOGRAM_F64, OTEL_HANDLE_KIND_HISTOGRAM_U64,
    OTEL_HANDLE_KIND_METER, OTEL_HANDLE_KIND_METER_PROVIDER,
    OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_F64, OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_U64,
    OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_F64, OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_I64,
    OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_U64, OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_F64,
    OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_I64, OTEL_HANDLE_KIND_UP_DOWN_COUNTER_F64,
    OTEL_HANDLE_KIND_UP_DOWN_COUNTER_I64,
};

use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::{
    checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasHandleHeader,
};
use crate::metrics_global::{retain_global_metrics, GlobalMetricsRetain};

const MAX_HISTOGRAM_BOUNDARIES: usize = 65_536;
const MAX_SCOPE_ATTRIBUTES: usize = 1_048_576;

pub(crate) enum MeterProviderInner {
    Global,
    Backed {
        vtable: *const OtelMetricsVtable,
        ctx: *mut c_void,
    },
}

#[repr(C)]
pub struct OtelMeterProvider {
    header: OtelHandleHeader,
    inner: MeterProviderInner,
}

impl OtelMeterProvider {
    pub(crate) fn new(inner: MeterProviderInner) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            inner,
        }
    }
}

impl HasHandleHeader for OtelMeterProvider {
    const KIND: u64 = OTEL_HANDLE_KIND_METER_PROVIDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

#[repr(C)]
pub struct OtelMeter {
    header: OtelHandleHeader,
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
}

impl HasHandleHeader for OtelMeter {
    const KIND: u64 = OTEL_HANDLE_KIND_METER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
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

/// Extensible instrumentation-scope options for meter acquisition.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMeterOptions {
    pub struct_size: u64,
    pub name: OtelStringView,
    pub version: OtelStringView,
    pub schema_url: OtelStringView,
    pub attributes: *const OtelKeyValue,
    pub attribute_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OtelMeterOptionsV1 {
    struct_size: u64,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
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
const OTEL_METER_OPTIONS_V1_SIZE: u64 = std::mem::size_of::<OtelMeterOptionsV1>() as u64;

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelInstrumentOptions>() == 56);
    assert!(std::mem::align_of::<OtelInstrumentOptions>() == 8);
    assert!(std::mem::size_of::<OtelMeterOptions>() == 72);
    assert!(std::mem::align_of::<OtelMeterOptions>() == 8);
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

pub(crate) unsafe fn validate_scope_attributes(
    attributes: *const OtelKeyValue,
    attribute_count: usize,
) -> Result<(), OtelStatus> {
    if attribute_count == 0 {
        return Ok(());
    }
    if attributes.is_null() {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "scope attribute array is NULL with non-zero count",
        ));
    }
    if attribute_count > MAX_SCOPE_ATTRIBUTES {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "scope attribute count exceeds the maximum supported value",
        ));
    }
    let valid_size = attribute_count
        .checked_mul(std::mem::size_of::<OtelKeyValue>())
        .is_some_and(|bytes| bytes <= isize::MAX as usize);
    if !valid_size {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "scope attribute array exceeds the maximum supported size",
        ));
    }
    let attributes = unsafe { std::slice::from_raw_parts(attributes, attribute_count) };
    let mut keys = HashSet::new();
    keys.try_reserve(attribute_count).map_err(|_| {
        fail(
            OtelStatus::InternalError,
            "failed to allocate scope attribute validation state",
        )
    })?;
    for attribute in attributes {
        let key = unsafe { attribute.key.as_str() }.map_err(fail_abi)?;
        if key.is_empty() {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "scope attribute key must not be empty",
            ));
        }
        if !keys.insert(key) {
            return Err(fail(
                OtelStatus::InvalidArgument,
                "duplicate scope attribute key",
            ));
        }
        let value_type = OtelAttributeType::from_u32(attribute.value_type).ok_or_else(|| {
            fail(
                OtelStatus::InvalidArgument,
                "unknown scope attribute value type",
            )
        })?;
        if value_type == OtelAttributeType::String {
            unsafe { attribute.value.string_value.as_str() }.map_err(fail_abi)?;
        }
    }
    Ok(())
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
    let options = OtelMeterOptions {
        struct_size: std::mem::size_of::<OtelMeterOptions>() as u64,
        name,
        version,
        schema_url,
        attributes: std::ptr::null(),
        attribute_count: 0,
    };
    unsafe { otel_meter_provider_get_meter_with_options(provider, &options) }
}

/// Obtain an owned meter from complete instrumentation-scope options.
///
/// # Safety
///
/// `provider` must be live. `options` must be readable for its declared prefix, every
/// non-empty string must address readable bytes, and a non-empty attribute array must
/// contain `attribute_count` readable values for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn otel_meter_provider_get_meter_with_options(
    provider: *const OtelMeterProvider,
    options: *const OtelMeterOptions,
) -> *mut OtelMeter {
    guard_ptr(|| {
        clear_last_error();
        let provider = match unsafe { checked_ref(provider) } {
            Some(provider) => provider,
            None => return std::ptr::null_mut(),
        };
        if options.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "meter options must not be NULL",
            );
            return std::ptr::null_mut();
        }
        let struct_size = unsafe { options.cast::<u64>().read() };
        if struct_size < OTEL_METER_OPTIONS_V1_SIZE {
            fail(
                OtelStatus::InvalidArgument,
                "meter options struct_size is smaller than the supported prefix",
            );
            return std::ptr::null_mut();
        }
        let options = unsafe { &*options.cast::<OtelMeterOptionsV1>() };
        if let Err(err) = unsafe {
            options
                .name
                .as_str()
                .and_then(|_| options.version.as_str())
                .and_then(|_| options.schema_url.as_str())
        } {
            fail_abi(err);
            return std::ptr::null_mut();
        }
        if unsafe { validate_scope_attributes(options.attributes, options.attribute_count) }
            .is_err()
        {
            return std::ptr::null_mut();
        }
        let (vtable, ctx, owned) = match &provider.inner {
            MeterProviderInner::Global => match retain_global_metrics() {
                GlobalMetricsRetain::NoProvider => {
                    return into_raw(OtelMeter {
                        header: OtelHandleHeader::new(OtelMeter::KIND),
                        vtable: std::ptr::null(),
                        ctx: std::ptr::null_mut(),
                    });
                }
                GlobalMetricsRetain::RetainFailed => return std::ptr::null_mut(),
                GlobalMetricsRetain::Retained { vtable, ctx } => (vtable, ctx, true),
            },
            MeterProviderInner::Backed { vtable, ctx } => (*vtable, *ctx, false),
        };
        let meter_ctx = if unsafe { metrics_vtable_supports_scope_config(vtable) } {
            let scope = OtelMetricScopeConfig {
                name: options.name,
                version: options.version,
                schema_url: options.schema_url,
                attributes: options.attributes,
                attribute_count: options.attribute_count,
            };
            unsafe { ((*vtable).provider_get_meter_with_scope)(ctx, &scope) }
        } else if options.attribute_count == 0 {
            unsafe {
                ((*vtable).provider_get_meter)(
                    ctx,
                    options.name,
                    options.version,
                    options.schema_url,
                )
            }
        } else {
            fail(
                OtelStatus::InvalidConfig,
                "registered Metrics SDK does not support scope attributes",
            );
            std::ptr::null_mut()
        };
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
            header: OtelHandleHeader::new(OtelMeter::KIND),
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
    let mut status = OtelStatus::Ok;
    let ctx = if unsafe { metrics_vtable_supports_creation_status(meter.vtable) } {
        unsafe {
            ((*meter.vtable).meter_create_instrument_with_status)(meter.ctx, &config, &mut status)
        }
    } else {
        unsafe { ((*meter.vtable).meter_create_instrument)(meter.ctx, &config) }
    };
    if ctx.is_null() {
        if !has_last_error() {
            set_last_error("backed metric instrument creation failed");
        }
        return Err(if status == OtelStatus::Ok {
            OtelStatus::InvalidConfig
        } else {
            status
        });
    }
    Ok((meter.vtable, ctx))
}

#[allow(unknown_lints)]
#[allow(edition_2024_expr_fragment_specifier)]
macro_rules! define_sync_instrument {
    (
        $handle:ident, $handle_kind:expr, $create:ident, $record:ident, $destroy_fn:ident,
        $kind:ident, $number:ident, $value:ty, $vtable_record:ident, $histogram:expr
    ) => {
        #[repr(C)]
        pub struct $handle {
            header: OtelHandleHeader,
            vtable: *const OtelMetricsVtable,
            ctx: *mut c_void,
        }

        impl HasHandleHeader for $handle {
            const KIND: u64 = $handle_kind;
            fn header(&self) -> &OtelHandleHeader {
                &self.header
            }
            fn header_mut(&mut self) -> &mut OtelHandleHeader {
                &mut self.header
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
                        header: OtelHandleHeader::new($handle::KIND),
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
    OTEL_HANDLE_KIND_COUNTER_U64,
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
    OTEL_HANDLE_KIND_COUNTER_F64,
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
    OTEL_HANDLE_KIND_UP_DOWN_COUNTER_I64,
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
    OTEL_HANDLE_KIND_UP_DOWN_COUNTER_F64,
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
    OTEL_HANDLE_KIND_GAUGE_U64,
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
    OTEL_HANDLE_KIND_GAUGE_I64,
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
    OTEL_HANDLE_KIND_GAUGE_F64,
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
    OTEL_HANDLE_KIND_HISTOGRAM_U64,
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
    OTEL_HANDLE_KIND_HISTOGRAM_F64,
    otel_meter_create_f64_histogram,
    otel_histogram_f64_record,
    otel_histogram_f64_destroy,
    Histogram,
    F64,
    f64,
    instrument_record_f64,
    true
);

macro_rules! define_bound_instrument {
    (
        $source:ident, $handle:ident, $handle_kind:expr, $bind:ident, $record:ident,
        $destroy_fn:ident, $value:ty, $vtable_record:ident
    ) => {
        #[repr(C)]
        pub struct $handle {
            header: OtelHandleHeader,
            vtable: *const OtelMetricsVtable,
            ctx: *mut c_void,
        }

        impl HasHandleHeader for $handle {
            const KIND: u64 = $handle_kind;
            fn header(&self) -> &OtelHandleHeader {
                &self.header
            }
            fn header_mut(&mut self) -> &mut OtelHandleHeader {
                &mut self.header
            }
        }

        unsafe impl Send for $handle {}
        unsafe impl Sync for $handle {}

        #[doc = "Bind an attribute set to a synchronous metric instrument."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be live. When `attribute_count` is non-zero, `attributes` \
                 must address that many readable values. `out` must address writable storage."]
        #[no_mangle]
        pub unsafe extern "C" fn $bind(
            instrument: *const $source,
            attributes: *const OtelKeyValue,
            attribute_count: usize,
            out: *mut *mut $handle,
        ) -> OtelStatus {
            guard_status(|| {
                clear_last_error();
                if out.is_null() {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "bound instrument out pointer is NULL",
                    );
                }
                unsafe { *out = std::ptr::null_mut() };
                let instrument = match unsafe { checked_ref(instrument) } {
                    Some(instrument) => instrument,
                    None => return OtelStatus::InvalidArgument,
                };
                if instrument.vtable.is_null() {
                    unsafe {
                        *out = into_raw($handle {
                            header: OtelHandleHeader::new($handle::KIND),
                            vtable: std::ptr::null(),
                            ctx: std::ptr::null_mut(),
                        });
                    }
                    return OtelStatus::Ok;
                }
                if !unsafe {
                    metrics_vtable_supports_bound_instruments(instrument.vtable)
                } {
                    return fail(
                        OtelStatus::InvalidConfig,
                        "installed Metrics SDK does not support bound instruments",
                    );
                }
                let mut status = OtelStatus::Ok;
                let ctx = unsafe {
                    ((*instrument.vtable).instrument_bind)(
                        instrument.ctx,
                        attributes,
                        attribute_count,
                        &mut status,
                    )
                };
                if status != OtelStatus::Ok {
                    if !ctx.is_null() {
                        unsafe { ((*instrument.vtable).bound_instrument_free)(ctx) };
                    }
                    return status;
                }
                if ctx.is_null() {
                    return fail(
                        OtelStatus::InternalError,
                        "Metrics SDK returned a NULL bound instrument",
                    );
                }
                unsafe {
                    *out = into_raw($handle {
                        header: OtelHandleHeader::new($handle::KIND),
                        vtable: instrument.vtable,
                        ctx,
                    });
                }
                OtelStatus::Ok
            })
        }

        #[doc = "Record through a bound metric instrument."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be a live handle of the exact expected type."]
        #[no_mangle]
        pub unsafe extern "C" fn $record(
            instrument: *const $handle,
            value: $value,
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
                unsafe { ((*instrument.vtable).$vtable_record)(instrument.ctx, value) }
            })
        }

        #[doc = "Destroy a bound metric instrument."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`instrument` must be NULL or a live handle, and destruction must not race \
                 with another use of that handle."]
        #[no_mangle]
        pub unsafe extern "C" fn $destroy_fn(instrument: *mut $handle) {
            guard_unit(|| {
                if let Some(instrument) = unsafe { checked_ref::<$handle>(instrument) } {
                    if !instrument.vtable.is_null() {
                        unsafe {
                            ((*instrument.vtable).bound_instrument_free)(instrument.ctx)
                        };
                    }
                }
                unsafe { destroy(instrument) };
            });
        }
    };
}

define_bound_instrument!(
    OtelCounterU64,
    OtelBoundCounterU64,
    OTEL_HANDLE_KIND_BOUND_COUNTER_U64,
    otel_counter_u64_bind,
    otel_bound_counter_u64_add,
    otel_bound_counter_u64_destroy,
    u64,
    bound_instrument_record_u64
);
define_bound_instrument!(
    OtelCounterF64,
    OtelBoundCounterF64,
    OTEL_HANDLE_KIND_BOUND_COUNTER_F64,
    otel_counter_f64_bind,
    otel_bound_counter_f64_add,
    otel_bound_counter_f64_destroy,
    f64,
    bound_instrument_record_f64
);
define_bound_instrument!(
    OtelHistogramU64,
    OtelBoundHistogramU64,
    OTEL_HANDLE_KIND_BOUND_HISTOGRAM_U64,
    otel_histogram_u64_bind,
    otel_bound_histogram_u64_record,
    otel_bound_histogram_u64_destroy,
    u64,
    bound_instrument_record_u64
);
define_bound_instrument!(
    OtelHistogramF64,
    OtelBoundHistogramF64,
    OTEL_HANDLE_KIND_BOUND_HISTOGRAM_F64,
    otel_histogram_f64_bind,
    otel_bound_histogram_f64_record,
    otel_bound_histogram_f64_destroy,
    f64,
    bound_instrument_record_f64
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
    destroy: Mutex<OtelUserDataDestroy>,
}

unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

impl Drop for UserData {
    fn drop(&mut self) {
        if let Some(destroy) = self
            .destroy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
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

    fn disable_and_relinquish_user_data(&self) {
        self.enabled.store(false, Ordering::Release);
        let user_data = self
            .user_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(user_data) = user_data {
            user_data
                .destroy
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
    }
}

#[derive(Clone, Copy)]
struct ObserverEntry {
    vtable: *const OtelMetricsVtable,
    ctx: *mut c_void,
    number: OtelMetricNumberKind,
}

static NEXT_OBSERVER: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    static OBSERVERS: RefCell<Vec<(usize, ObserverEntry)>> = const { RefCell::new(Vec::new()) };
}

fn next_observer_token() -> usize {
    loop {
        let token = NEXT_OBSERVER.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            return token;
        }
    }
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
        let token = next_observer_token();
        OBSERVERS.with(|observers| {
            observers.borrow_mut().push((
                token,
                ObserverEntry {
                    vtable,
                    ctx,
                    number,
                },
            ));
        });
        Self { token }
    }
}

impl Drop for ObserverRegistration {
    fn drop(&mut self) {
        OBSERVERS.with(|observers| {
            let mut observers = observers.borrow_mut();
            if let Some(index) = observers
                .iter()
                .rposition(|(token, _)| *token == self.token)
            {
                observers.remove(index);
            }
        });
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
    let entry = OBSERVERS.with(|observers| {
        observers
            .borrow()
            .iter()
            .rev()
            .find_map(|(candidate, entry)| (*candidate == token).then_some(*entry))
    });
    // The callback stack owns this thread-local entry until callback return. Copying it lets
    // SDK dispatch reenter observer APIs without retaining a RefCell borrow across FFI.
    let entry = match entry {
        Some(entry) if entry.number == expected && !entry.vtable.is_null() => entry,
        _ => {
            return fail(
                OtelStatus::InvalidArgument,
                "observer is not active for this callback invocation",
            )
        }
    };
    let _ = (attributes, attribute_count);
    call(&entry, value)
}

/// Record a value through a callback-scoped unsigned observer.
///
/// # Safety
///
/// `observer` must be the token passed to the currently executing callback on this thread. When
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
/// `observer` must be the token passed to the currently executing callback on this thread. When
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
/// `observer` must be the token passed to the currently executing callback on this thread. When
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
    let mut status = OtelStatus::Ok;
    let ctx = if unsafe { metrics_vtable_supports_creation_status(meter.vtable) } {
        unsafe {
            ((*meter.vtable).meter_create_instrument_with_status)(meter.ctx, &config, &mut status)
        }
    } else {
        unsafe { ((*meter.vtable).meter_create_instrument)(meter.ctx, &config) }
    };
    if ctx.is_null() {
        if !has_last_error() {
            set_last_error("backed observable metric instrument creation failed");
        }
        return Err(if status == OtelStatus::Ok {
            OtelStatus::InvalidConfig
        } else {
            status
        });
    }
    Ok((meter.vtable, ctx))
}

#[allow(unknown_lints)]
#[allow(edition_2024_expr_fragment_specifier)]
macro_rules! define_observable_instrument {
    (
        $handle:ident, $handle_kind:expr, $create:ident, $destroy_fn:ident,
        $kind:ident, $number:ident, $callback_ty:ty, $callback_variant:ident,
        $trampoline:ident
    ) => {
        #[repr(C)]
        pub struct $handle {
            header: OtelHandleHeader,
            vtable: *const OtelMetricsVtable,
            ctx: *mut c_void,
            state: Arc<CallbackState>,
        }

        impl HasHandleHeader for $handle {
            const KIND: u64 = $handle_kind;
            fn header(&self) -> &OtelHandleHeader {
                &self.header
            }
            fn header_mut(&mut self) -> &mut OtelHandleHeader {
                &mut self.header
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
                        destroy: Mutex::new(user_data_destroy),
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
                    Err(status) => {
                        state.disable_and_relinquish_user_data();
                        return status;
                    }
                };
                unsafe {
                    *out = into_raw($handle {
                        header: OtelHandleHeader::new($handle::KIND),
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
    OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_U64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_F64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_I64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_F64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_U64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_I64,
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
    OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_F64,
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
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex, OnceLock};
    use std::time::Duration;

    use opentelemetry_c_abi::OTEL_METRICS_IMPL_ABI_VERSION;

    struct MockInstrument {
        callback: extern "C" fn(*mut c_void, *mut c_void),
        state: *mut c_void,
        state_free: extern "C" fn(*mut c_void),
    }

    thread_local! {
        static FAIL_OBSERVABLE_CREATION: Cell<bool> = const { Cell::new(false) };
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

    extern "C" fn mock_provider_get_meter_with_scope(
        _provider_ctx: *mut c_void,
        _scope: *const OtelMetricScopeConfig,
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
        if FAIL_OBSERVABLE_CREATION.with(|fail| fail.replace(false)) {
            state_free(config.callback_state);
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(MockInstrument {
            callback,
            state: config.callback_state,
            state_free,
        }))
        .cast()
    }

    extern "C" fn mock_meter_create_instrument_with_status(
        meter_ctx: *mut c_void,
        config: *const OtelMetricInstrumentConfig,
        out_status: *mut OtelStatus,
    ) -> *mut c_void {
        let instrument = mock_meter_create_instrument(meter_ctx, config);
        if !out_status.is_null() {
            unsafe {
                *out_status = if instrument.is_null() {
                    OtelStatus::InvalidConfig
                } else {
                    OtelStatus::Ok
                };
            }
        }
        instrument
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

    extern "C" fn mock_bind(
        _ctx: *mut c_void,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
        out_status: *mut OtelStatus,
    ) -> *mut c_void {
        if !out_status.is_null() {
            unsafe { *out_status = OtelStatus::InvalidConfig };
        }
        std::ptr::null_mut()
    }

    extern "C" fn mock_bind_null_ok(
        _ctx: *mut c_void,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
        out_status: *mut OtelStatus,
    ) -> *mut c_void {
        if !out_status.is_null() {
            unsafe { *out_status = OtelStatus::Ok };
        }
        std::ptr::null_mut()
    }

    static INCONSISTENT_BOUND_FREES: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn mock_bind_context_with_error(
        _ctx: *mut c_void,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
        out_status: *mut OtelStatus,
    ) -> *mut c_void {
        if !out_status.is_null() {
            unsafe { *out_status = OtelStatus::InvalidUtf8 };
        }
        Box::into_raw(Box::new(0_u8)).cast()
    }

    extern "C" fn mock_inconsistent_bound_free(ctx: *mut c_void) {
        if !ctx.is_null() {
            drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
            INCONSISTENT_BOUND_FREES.fetch_add(1, Ordering::SeqCst);
        }
    }

    extern "C" fn mock_bound_record_u64(_ctx: *mut c_void, _value: u64) -> OtelStatus {
        OtelStatus::Ok
    }

    extern "C" fn mock_bound_record_f64(_ctx: *mut c_void, _value: f64) -> OtelStatus {
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
        abi_version: OTEL_METRICS_IMPL_ABI_VERSION,
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
        provider_get_meter_with_scope: mock_provider_get_meter_with_scope,
        meter_create_instrument_with_status: mock_meter_create_instrument_with_status,
        instrument_bind: mock_bind,
        bound_instrument_record_u64: mock_bound_record_u64,
        bound_instrument_record_f64: mock_bound_record_f64,
        bound_instrument_free: mock_free,
    };

    fn metrics_vtable_with_observer_u64(
        observer_observe_u64: extern "C" fn(
            *mut c_void,
            u64,
            *const OtelKeyValue,
            usize,
        ) -> OtelStatus,
    ) -> OtelMetricsVtable {
        OtelMetricsVtable {
            observer_observe_u64,
            ..MOCK_METRICS_VTABLE
        }
    }

    #[test]
    fn bound_instrument_api_rejects_incompatible_and_malformed_results() {
        let counter = |vtable: *const OtelMetricsVtable| OtelCounterU64 {
            header: OtelHandleHeader::new(OtelCounterU64::KIND),
            vtable,
            ctx: std::ptr::NonNull::<c_void>::dangling().as_ptr(),
        };
        let mut out = std::ptr::null_mut();

        assert_eq!(
            unsafe {
                otel_counter_u64_bind(
                    &counter(&MOCK_METRICS_VTABLE),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                )
            },
            OtelStatus::InvalidArgument
        );

        let scope_only_vtable = OtelMetricsVtable {
            struct_size: opentelemetry_c_abi::OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE,
            ..MOCK_METRICS_VTABLE
        };
        assert_eq!(
            unsafe {
                otel_counter_u64_bind(&counter(&scope_only_vtable), std::ptr::null(), 0, &mut out)
            },
            OtelStatus::InvalidConfig
        );
        assert!(out.is_null());

        let null_ok_vtable = OtelMetricsVtable {
            instrument_bind: mock_bind_null_ok,
            ..MOCK_METRICS_VTABLE
        };
        assert_eq!(
            unsafe {
                otel_counter_u64_bind(&counter(&null_ok_vtable), std::ptr::null(), 0, &mut out)
            },
            OtelStatus::InternalError
        );
        assert!(out.is_null());

        INCONSISTENT_BOUND_FREES.store(0, Ordering::SeqCst);
        let context_with_error_vtable = OtelMetricsVtable {
            instrument_bind: mock_bind_context_with_error,
            bound_instrument_free: mock_inconsistent_bound_free,
            ..MOCK_METRICS_VTABLE
        };
        assert_eq!(
            unsafe {
                otel_counter_u64_bind(
                    &counter(&context_with_error_vtable),
                    std::ptr::null(),
                    0,
                    &mut out,
                )
            },
            OtelStatus::InvalidUtf8
        );
        assert!(out.is_null());
        assert_eq!(INCONSISTENT_BOUND_FREES.load(Ordering::SeqCst), 1);

        assert_eq!(
            unsafe {
                otel_counter_u64_bind(
                    &counter(&MOCK_METRICS_VTABLE),
                    std::ptr::null(),
                    0,
                    &mut out,
                )
            },
            OtelStatus::InvalidConfig
        );
        assert!(out.is_null());

        let bound_histogram = OtelBoundHistogramU64 {
            header: OtelHandleHeader::new(OtelBoundHistogramU64::KIND),
            vtable: std::ptr::null(),
            ctx: std::ptr::null_mut(),
        };
        assert_eq!(
            unsafe {
                otel_bound_counter_u64_add(
                    (&bound_histogram as *const OtelBoundHistogramU64).cast(),
                    1,
                )
            },
            OtelStatus::InvalidArgument
        );
    }

    struct ConcurrentObserverProbe {
        entered: Mutex<usize>,
        both_entered: Condvar,
    }

    extern "C" fn concurrent_observer_record(
        ctx: *mut c_void,
        _value: u64,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
    ) -> OtelStatus {
        let probe = unsafe { &*(ctx.cast::<ConcurrentObserverProbe>()) };
        let mut entered = probe
            .entered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *entered += 1;
        probe.both_entered.notify_all();
        let (entered, timeout) = probe
            .both_entered
            .wait_timeout_while(entered, Duration::from_secs(1), |entered| *entered < 2)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if timeout.timed_out() && *entered < 2 {
            OtelStatus::InternalError
        } else {
            OtelStatus::Ok
        }
    }

    struct ReentrantObserverProbe {
        token: AtomicUsize,
        calls: AtomicUsize,
    }

    extern "C" fn reentrant_observer_record(
        ctx: *mut c_void,
        _value: u64,
        _attributes: *const OtelKeyValue,
        _attribute_count: usize,
    ) -> OtelStatus {
        let probe = unsafe { &*(ctx.cast::<ReentrantObserverProbe>()) };
        if probe.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            unsafe {
                otel_observer_u64_observe(
                    probe.token.load(Ordering::SeqCst) as *mut OtelObserverU64,
                    2,
                    std::ptr::null(),
                    0,
                )
            }
        } else {
            OtelStatus::Ok
        }
    }

    #[test]
    fn observer_tokens_are_thread_bound_and_expire() {
        let registration = ObserverRegistration::new(
            &MOCK_METRICS_VTABLE,
            std::ptr::null_mut(),
            OtelMetricNumberKind::U64,
        );
        let token = registration.token;

        assert_eq!(
            unsafe {
                otel_observer_u64_observe(token as *mut OtelObserverU64, 1, std::ptr::null(), 0)
            },
            OtelStatus::Ok
        );
        assert_eq!(
            std::thread::spawn(move || unsafe {
                otel_observer_u64_observe(token as *mut OtelObserverU64, 1, std::ptr::null(), 0)
            })
            .join()
            .unwrap(),
            OtelStatus::InvalidArgument
        );

        drop(registration);
        assert_eq!(
            unsafe {
                otel_observer_u64_observe(token as *mut OtelObserverU64, 1, std::ptr::null(), 0)
            },
            OtelStatus::InvalidArgument
        );
    }

    #[test]
    fn observers_on_different_collection_threads_are_not_serialized() {
        let probe = Arc::new(ConcurrentObserverProbe {
            entered: Mutex::new(0),
            both_entered: Condvar::new(),
        });
        let vtable = Arc::new(metrics_vtable_with_observer_u64(concurrent_observer_record));

        let threads: Vec<_> = (0..2)
            .map(|_| {
                let probe = Arc::clone(&probe);
                let vtable = Arc::clone(&vtable);
                std::thread::spawn(move || {
                    let registration = ObserverRegistration::new(
                        Arc::as_ptr(&vtable),
                        Arc::as_ptr(&probe) as *mut c_void,
                        OtelMetricNumberKind::U64,
                    );
                    unsafe {
                        otel_observer_u64_observe(
                            registration.token as *mut OtelObserverU64,
                            1,
                            std::ptr::null(),
                            0,
                        )
                    }
                })
            })
            .collect();

        for thread in threads {
            assert_eq!(thread.join().unwrap(), OtelStatus::Ok);
        }
        assert_eq!(
            *probe
                .entered
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            2
        );
    }

    #[test]
    fn observer_dispatch_allows_same_thread_reentrancy() {
        let probe = ReentrantObserverProbe {
            token: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        };
        let vtable = metrics_vtable_with_observer_u64(reentrant_observer_record);
        let registration = ObserverRegistration::new(
            &vtable,
            &probe as *const ReentrantObserverProbe as *mut c_void,
            OtelMetricNumberKind::U64,
        );
        probe.token.store(registration.token, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                otel_observer_u64_observe(
                    registration.token as *mut OtelObserverU64,
                    1,
                    std::ptr::null(),
                    0,
                )
            },
            OtelStatus::Ok
        );
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }

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

    extern "C" fn count_user_data_destroy(user_data: *mut c_void) {
        let destroyed = unsafe { Box::from_raw(user_data.cast::<Arc<AtomicUsize>>()) };
        destroyed.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn no_op_observable_callback(
        _observer: *mut OtelObserverU64,
        _user_data: *mut c_void,
    ) {
    }

    #[test]
    fn observable_creation_failure_preserves_caller_user_data_ownership() {
        let meter = OtelMeter {
            header: OtelHandleHeader::new(OtelMeter::KIND),
            vtable: &MOCK_METRICS_VTABLE,
            ctx: std::ptr::NonNull::<c_void>::dangling().as_ptr(),
        };
        let destroyed = Arc::new(AtomicUsize::new(0));
        let user_data = Box::into_raw(Box::new(Arc::clone(&destroyed))).cast();
        let mut observable = std::ptr::NonNull::<OtelObservableGaugeU64>::dangling().as_ptr();

        FAIL_OBSERVABLE_CREATION.with(|fail| fail.set(true));
        assert_eq!(
            unsafe {
                otel_meter_create_u64_observable_gauge(
                    &meter,
                    OtelStringView {
                        ptr: b"observable".as_ptr().cast(),
                        len: 10,
                    },
                    std::ptr::null(),
                    Some(no_op_observable_callback),
                    user_data,
                    Some(count_user_data_destroy),
                    &mut observable,
                )
            },
            OtelStatus::InvalidConfig
        );
        assert!(observable.is_null());
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);

        count_user_data_destroy(user_data);
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn observable_destroy_defers_user_data_until_in_flight_callback_completes() {
        let (reached, _) = free_reached();
        *reached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;

        let meter = OtelMeter {
            header: OtelHandleHeader::new(OtelMeter::KIND),
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
