//! Declarative Metrics view builder.

use opentelemetry::Key;
use opentelemetry_c_abi::{OtelBool, OtelMetricInstrumentKind, OtelStatus, OtelStringView};
use opentelemetry_sdk::metrics::{Aggregation, Instrument, InstrumentKind, Stream};

use crate::error::{clear_last_error, fail, fail_abi, fail_owned};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasMagic,
};

const BUILDER_MAGIC: u64 = 0x4F54_4C43_4D56_4942;
const VIEW_MAGIC: u64 = 0x4F54_4C43_4D56_4945;
const ANY_KIND: u32 = u32::MAX;

#[derive(Clone)]
pub(crate) enum AggregationConfig {
    Default,
    Drop,
    Sum,
    LastValue,
    ExplicitHistogram {
        boundaries: Vec<f64>,
        record_min_max: bool,
    },
    ExponentialHistogram {
        max_size: u32,
        max_scale: i8,
        record_min_max: bool,
    },
}

#[derive(Clone)]
pub(crate) struct MetricViewConfig {
    name_pattern: Option<String>,
    meter_name: Option<String>,
    unit: Option<String>,
    kind: u32,
    output_name: Option<String>,
    output_description: Option<String>,
    output_unit: Option<String>,
    attribute_filter_enabled: bool,
    allowed_attributes: Vec<String>,
    cardinality_limit: Option<usize>,
    aggregation: AggregationConfig,
}

pub struct OtelMetricViewBuilder {
    magic: u64,
    config: MetricViewConfig,
}

pub struct OtelMetricView {
    magic: u64,
    pub(crate) config: MetricViewConfig,
}

impl HasMagic for OtelMetricViewBuilder {
    const MAGIC: u64 = BUILDER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

impl HasMagic for OtelMetricView {
    const MAGIC: u64 = VIEW_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

fn default_config() -> MetricViewConfig {
    MetricViewConfig {
        name_pattern: None,
        meter_name: None,
        unit: None,
        kind: ANY_KIND,
        output_name: None,
        output_description: None,
        output_unit: None,
        attribute_filter_enabled: false,
        allowed_attributes: Vec::new(),
        cardinality_limit: None,
        aggregation: AggregationConfig::Default,
    }
}

#[no_mangle]
pub extern "C" fn otel_metric_view_builder_new() -> *mut OtelMetricViewBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelMetricViewBuilder {
            magic: BUILDER_MAGIC,
            config: default_config(),
        })
    })
}

/// Destroy a Metrics view builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_destroy(builder: *mut OtelMetricViewBuilder) {
    guard_unit(|| unsafe { destroy(builder) });
}

unsafe fn with_builder(
    builder: *mut OtelMetricViewBuilder,
    f: impl FnOnce(&mut MetricViewConfig) -> OtelStatus,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        match unsafe { checked_mut(builder) } {
            Some(builder) => f(&mut builder.config),
            None => OtelStatus::InvalidArgument,
        }
    })
}

unsafe fn set_optional_string(
    builder: *mut OtelMetricViewBuilder,
    value: OtelStringView,
    set: impl FnOnce(&mut MetricViewConfig, Option<String>),
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| match value.to_string_strict() {
            Ok(value) => {
                set(config, (!value.is_empty()).then_some(value));
                OtelStatus::Ok
            }
            Err(err) => fail_abi(err),
        })
    }
}

macro_rules! string_setter {
    ($name:ident, $field:ident) => {
        #[doc = "Set an optional string field on a Metrics view builder."]
        #[doc = ""]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "`builder` must be live and the string view must address readable bytes for \
                 the duration of the call."]
        #[no_mangle]
        pub unsafe extern "C" fn $name(
            builder: *mut OtelMetricViewBuilder,
            value: OtelStringView,
        ) -> OtelStatus {
            unsafe { set_optional_string(builder, value, |config, value| config.$field = value) }
        }
    };
}

string_setter!(otel_metric_view_builder_set_name_pattern, name_pattern);
string_setter!(otel_metric_view_builder_set_meter_name, meter_name);
string_setter!(otel_metric_view_builder_set_unit, unit);
string_setter!(otel_metric_view_builder_set_output_name, output_name);
string_setter!(
    otel_metric_view_builder_set_output_description,
    output_description
);
string_setter!(otel_metric_view_builder_set_output_unit, output_unit);

/// Set the selected instrument kind.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_instrument_kind(
    builder: *mut OtelMetricViewBuilder,
    kind: u32,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            if kind != ANY_KIND && OtelMetricInstrumentKind::from_u32(kind).is_none() {
                return fail(
                    OtelStatus::InvalidArgument,
                    "unknown metric instrument kind",
                );
            }
            config.kind = kind;
            OtelStatus::Ok
        })
    }
}

/// Add an allowed attribute key.
///
/// # Safety
///
/// `builder` must be live and `key` must address readable bytes for the call.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_add_allowed_attribute(
    builder: *mut OtelMetricViewBuilder,
    key: OtelStringView,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| match key.to_string_strict() {
            Ok(key) if !key.is_empty() => {
                config.attribute_filter_enabled = true;
                config.allowed_attributes.push(key);
                OtelStatus::Ok
            }
            Ok(_) => fail(
                OtelStatus::InvalidArgument,
                "allowed attribute key must not be empty",
            ),
            Err(err) => fail_abi(err),
        })
    }
}

/// Enable or disable attribute filtering for the selected stream.
///
/// Enabling with no allowed keys intentionally drops every attribute.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_attribute_filter_enabled(
    builder: *mut OtelMetricViewBuilder,
    enabled: OtelBool,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            config.attribute_filter_enabled = enabled != 0;
            OtelStatus::Ok
        })
    }
}

/// Set the cardinality limit.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_cardinality_limit(
    builder: *mut OtelMetricViewBuilder,
    limit: u64,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            let limit = match usize::try_from(limit) {
                Ok(limit) if limit > 0 && limit < usize::MAX => limit,
                _ => {
                    return fail(
                        OtelStatus::InvalidConfig,
                        "cardinality limit must be between 1 and usize::MAX - 1",
                    )
                }
            };
            config.cardinality_limit = Some(limit);
            OtelStatus::Ok
        })
    }
}

/// Select a non-parameterized aggregation.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_aggregation(
    builder: *mut OtelMetricViewBuilder,
    aggregation: u32,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            config.aggregation = match aggregation {
                0 => AggregationConfig::Default,
                1 => AggregationConfig::Drop,
                2 => AggregationConfig::Sum,
                3 => AggregationConfig::LastValue,
                _ => {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "aggregation requires explicit histogram configuration or is unknown",
                    )
                }
            };
            OtelStatus::Ok
        })
    }
}

fn copy_boundaries(boundaries: *const f64, count: usize) -> Result<Vec<f64>, OtelStatus> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let valid_size = count
        .checked_mul(std::mem::size_of::<f64>())
        .is_some_and(|bytes| bytes <= isize::MAX as usize);
    if boundaries.is_null() || !valid_size {
        return Err(fail(
            OtelStatus::InvalidArgument,
            "invalid explicit histogram boundary array",
        ));
    }
    let values = unsafe { std::slice::from_raw_parts(boundaries, count) };
    if values.iter().any(|value| !value.is_finite())
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "explicit histogram boundaries must be finite and strictly increasing",
        ));
    }
    Ok(values.to_vec())
}

/// Configure explicit histogram aggregation.
///
/// # Safety
///
/// `builder` must be live. When `count` is non-zero, `boundaries` must address that many
/// readable values.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_explicit_histogram(
    builder: *mut OtelMetricViewBuilder,
    boundaries: *const f64,
    count: usize,
    record_min_max: u32,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            let boundaries = match copy_boundaries(boundaries, count) {
                Ok(boundaries) => boundaries,
                Err(status) => return status,
            };
            config.aggregation = AggregationConfig::ExplicitHistogram {
                boundaries,
                record_min_max: record_min_max != 0,
            };
            OtelStatus::Ok
        })
    }
}

/// Configure base-2 exponential histogram aggregation.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_set_exponential_histogram(
    builder: *mut OtelMetricViewBuilder,
    max_size: u32,
    max_scale: i8,
    record_min_max: u32,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            if max_size == 0 || !(-10..=20).contains(&max_scale) {
                return fail(
                    OtelStatus::InvalidConfig,
                    "exponential histogram requires max_size > 0 and max_scale in [-10, 20]",
                );
            }
            config.aggregation = AggregationConfig::ExponentialHistogram {
                max_size,
                max_scale,
                record_min_max: record_min_max != 0,
            };
            OtelStatus::Ok
        })
    }
}

fn stream(config: &MetricViewConfig) -> Result<Stream, String> {
    let mut builder = Stream::builder();
    if let Some(name) = &config.output_name {
        builder = builder.with_name(name.clone());
    }
    if let Some(description) = &config.output_description {
        builder = builder.with_description(description.clone());
    }
    if let Some(unit) = &config.output_unit {
        builder = builder.with_unit(unit.clone());
    }
    if config.attribute_filter_enabled {
        builder = builder
            .with_allowed_attribute_keys(config.allowed_attributes.iter().cloned().map(Key::from));
    }
    if let Some(limit) = config.cardinality_limit {
        builder = builder.with_cardinality_limit(limit);
    }
    let aggregation = match &config.aggregation {
        AggregationConfig::Default => Aggregation::Default,
        AggregationConfig::Drop => Aggregation::Drop,
        AggregationConfig::Sum => Aggregation::Sum,
        AggregationConfig::LastValue => Aggregation::LastValue,
        AggregationConfig::ExplicitHistogram {
            boundaries,
            record_min_max,
        } => Aggregation::ExplicitBucketHistogram {
            boundaries: boundaries.clone(),
            record_min_max: *record_min_max,
        },
        AggregationConfig::ExponentialHistogram {
            max_size,
            max_scale,
            record_min_max,
        } => Aggregation::Base2ExponentialHistogram {
            max_size: *max_size,
            max_scale: *max_scale,
            record_min_max: *record_min_max,
        },
    };
    builder
        .with_aggregation(aggregation)
        .build()
        .map_err(|err| err.to_string())
}

fn kind_matches(config: u32, actual: InstrumentKind) -> bool {
    if config == ANY_KIND {
        return true;
    }
    matches!(
        (OtelMetricInstrumentKind::from_u32(config), actual),
        (
            Some(OtelMetricInstrumentKind::Counter),
            InstrumentKind::Counter
        ) | (
            Some(OtelMetricInstrumentKind::UpDownCounter),
            InstrumentKind::UpDownCounter
        ) | (Some(OtelMetricInstrumentKind::Gauge), InstrumentKind::Gauge)
            | (
                Some(OtelMetricInstrumentKind::Histogram),
                InstrumentKind::Histogram
            )
            | (
                Some(OtelMetricInstrumentKind::ObservableCounter),
                InstrumentKind::ObservableCounter
            )
            | (
                Some(OtelMetricInstrumentKind::ObservableUpDownCounter),
                InstrumentKind::ObservableUpDownCounter
            )
            | (
                Some(OtelMetricInstrumentKind::ObservableGauge),
                InstrumentKind::ObservableGauge
            )
    )
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some(star) = pattern.find('*') else {
        return pattern == value;
    };
    let (prefix, suffix_with_star) = pattern.split_at(star);
    let suffix = &suffix_with_star[1..];
    value.starts_with(prefix)
        && value.ends_with(suffix)
        && value.len() >= prefix.len() + suffix.len()
}

impl MetricViewConfig {
    pub(crate) fn apply(&self, instrument: &Instrument) -> Option<Stream> {
        if self
            .name_pattern
            .as_ref()
            .is_some_and(|pattern| !wildcard_matches(pattern, instrument.name()))
            || self
                .meter_name
                .as_ref()
                .is_some_and(|name| name != instrument.scope().name())
            || self
                .unit
                .as_ref()
                .is_some_and(|unit| unit != instrument.unit())
            || !kind_matches(self.kind, instrument.kind())
        {
            return None;
        }
        stream(self).ok()
    }
}

/// Build an owned Metrics view.
///
/// # Safety
///
/// `builder` must be live and `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_build(
    builder: *mut OtelMetricViewBuilder,
    out: *mut *mut OtelMetricView,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let builder = match unsafe { checked_mut(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        if let Err(err) = stream(&builder.config) {
            return fail_owned(
                OtelStatus::InvalidConfig,
                format!("invalid metric view stream: {err}"),
            );
        }
        unsafe {
            *out = into_raw(OtelMetricView {
                magic: VIEW_MAGIC,
                config: builder.config.clone(),
            })
        };
        OtelStatus::Ok
    })
}

/// Destroy a Metrics view.
///
/// # Safety
///
/// `view` must be NULL or a live view and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_destroy(view: *mut OtelMetricView) {
    guard_unit(|| unsafe { destroy(view) });
}
