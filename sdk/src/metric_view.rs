//! Declarative Metrics view builder.

use opentelemetry::{Key, KeyValue};
use opentelemetry_c_abi::{
    OtelBool, OtelHandleHeader, OtelKeyValue, OtelMetricInstrumentKind, OtelStatus, OtelStringView,
    OTEL_HANDLE_KIND_METRIC_VIEW, OTEL_HANDLE_KIND_METRIC_VIEW_BUILDER,
};
use opentelemetry_sdk::metrics::{Aggregation, Instrument, InstrumentKind, Stream};

use crate::error::{clear_last_error, fail, fail_abi, fail_owned};
use crate::handle::{
    checked_mut, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasHandleHeader,
};

const ANY_KIND: u32 = u32::MAX;
const MAX_VIEW_SCOPE_ATTRIBUTES: usize = 256;
const MAX_VIEW_ALLOWED_ATTRIBUTES: usize = 1024;

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
    scope_version: Option<String>,
    scope_schema_url: Option<String>,
    scope_attributes: Vec<KeyValue>,
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

#[repr(C)]
pub struct OtelMetricViewBuilder {
    header: OtelHandleHeader,
    config: MetricViewConfig,
}

#[repr(C)]
pub struct OtelMetricView {
    header: OtelHandleHeader,
    pub(crate) config: MetricViewConfig,
}

impl HasHandleHeader for OtelMetricViewBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_METRIC_VIEW_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

impl HasHandleHeader for OtelMetricView {
    const KIND: u64 = OTEL_HANDLE_KIND_METRIC_VIEW;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

fn default_config() -> MetricViewConfig {
    MetricViewConfig {
        name_pattern: None,
        meter_name: None,
        scope_version: None,
        scope_schema_url: None,
        scope_attributes: Vec::new(),
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
            header: OtelHandleHeader::new(OtelMetricViewBuilder::KIND),
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
string_setter!(otel_metric_view_builder_set_scope_version, scope_version);
string_setter!(
    otel_metric_view_builder_set_scope_schema_url,
    scope_schema_url
);
string_setter!(otel_metric_view_builder_set_unit, unit);
string_setter!(otel_metric_view_builder_set_output_name, output_name);
string_setter!(
    otel_metric_view_builder_set_output_description,
    output_description
);
string_setter!(otel_metric_view_builder_set_output_unit, output_unit);

/// Add a required exact instrumentation-scope attribute matcher.
///
/// All configured scope attributes must be present with the same type and value for the
/// view to match. Additional scope attributes do not prevent a match.
///
/// # Safety
///
/// `builder` and `attribute` must be live and readable for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_builder_add_scope_attribute(
    builder: *mut OtelMetricViewBuilder,
    attribute: *const OtelKeyValue,
) -> OtelStatus {
    unsafe {
        with_builder(builder, |config| {
            if attribute.is_null() {
                return fail(
                    OtelStatus::InvalidArgument,
                    "scope attribute must not be NULL",
                );
            }
            if config.scope_attributes.len() >= MAX_VIEW_SCOPE_ATTRIBUTES {
                return fail(
                    OtelStatus::InvalidConfig,
                    "Metrics view scope attribute limit exceeded",
                );
            }
            if config.scope_attributes.try_reserve(1).is_err() {
                return fail(
                    OtelStatus::InternalError,
                    "failed to allocate space for a Metrics view scope attribute",
                );
            }
            let attribute = match crate::vtable::to_key_value(&*attribute) {
                Ok(attribute) => attribute,
                Err(status) => return status,
            };
            if config
                .scope_attributes
                .iter()
                .any(|existing| existing.key == attribute.key)
            {
                return fail(
                    OtelStatus::InvalidArgument,
                    "duplicate scope attribute matcher key",
                );
            }
            config.scope_attributes.push(attribute);
            OtelStatus::Ok
        })
    }
}

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
        with_builder(builder, |config| {
            if config.allowed_attributes.len() >= MAX_VIEW_ALLOWED_ATTRIBUTES {
                return fail(
                    OtelStatus::InvalidConfig,
                    "Metrics view allowed attribute limit exceeded",
                );
            }
            if config.allowed_attributes.try_reserve(1).is_err() {
                return fail(
                    OtelStatus::InternalError,
                    "failed to allocate space for a Metrics view allowed attribute",
                );
            }
            match key.to_string_strict() {
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
            }
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
                .scope_version
                .as_ref()
                .is_some_and(|version| instrument.scope().version() != Some(version.as_str()))
            || self.scope_schema_url.as_ref().is_some_and(|schema_url| {
                instrument.scope().schema_url() != Some(schema_url.as_str())
            })
            || self.scope_attributes.iter().any(|expected| {
                !instrument
                    .scope()
                    .attributes()
                    .any(|actual| actual == expected)
            })
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
        if builder
            .config
            .name_pattern
            .as_ref()
            .is_some_and(|pattern| pattern.bytes().filter(|byte| *byte == b'*').count() > 1)
        {
            return fail(
                OtelStatus::InvalidConfig,
                "metric view name pattern supports at most one '*'",
            );
        }
        if let Err(err) = stream(&builder.config) {
            return fail_owned(
                OtelStatus::InvalidConfig,
                format!("invalid metric view stream: {err}"),
            );
        }
        unsafe {
            *out = into_raw(OtelMetricView {
                header: OtelHandleHeader::new(OtelMetricView::KIND),
                config: builder.config.clone(),
            })
        };
        OtelStatus::Ok
    })
}

/// Destroy an untransferred Metrics view. After a successful transfer into an SDK builder, the
/// original pointer is invalid and must not be passed here.
///
/// # Safety
///
/// `view` must be NULL or a live view and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_view_destroy(view: *mut OtelMetricView) {
    guard_unit(|| unsafe { destroy(view) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::{InstrumentationScope, KeyValue, Value};
    use opentelemetry_c_abi::{OtelAttributeType, OtelAttributeValue};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    fn collect_with_views(
        views: Vec<MetricViewConfig>,
        record: impl FnOnce(&SdkMeterProvider),
    ) -> Vec<ResourceMetrics> {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let mut builder = SdkMeterProvider::builder().with_reader(reader);
        for view in views {
            builder = builder.with_view(move |instrument| view.apply(instrument));
        }
        let provider = builder.build();
        record(&provider);
        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        provider.shutdown().unwrap();
        metrics
    }

    fn find_metric<'a>(
        metrics: &'a [ResourceMetrics],
        scope_name: &str,
        name: &str,
    ) -> &'a opentelemetry_sdk::metrics::data::Metric {
        metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .filter(|scope| scope.scope().name() == scope_name)
            .flat_map(|scope| scope.metrics())
            .find(|metric| metric.name() == name)
            .unwrap_or_else(|| panic!("missing metric {scope_name}/{name}"))
    }

    #[test]
    fn matcher_selection_stream_overrides_and_attribute_allow_list_apply() {
        let mut view = default_config();
        view.name_pattern = Some("requests_*".to_string());
        view.meter_name = Some("matching_meter".to_string());
        view.unit = Some("ms".to_string());
        view.kind = OtelMetricInstrumentKind::Counter as u32;
        view.output_name = Some("renamed_requests".to_string());
        view.output_description = Some("renamed description".to_string());
        view.output_unit = Some("requests".to_string());
        view.attribute_filter_enabled = true;
        view.allowed_attributes = vec!["route".to_string()];
        view.aggregation = AggregationConfig::Sum;

        let metrics = collect_with_views(vec![view], |provider| {
            let matching = provider.meter("matching_meter");
            matching
                .u64_counter("requests_total")
                .with_unit("ms")
                .build()
                .add(
                    5,
                    &[
                        KeyValue::new("route", "/items"),
                        KeyValue::new("ignored", "value"),
                    ],
                );
            provider
                .meter("other_meter")
                .u64_counter("requests_total")
                .with_unit("ms")
                .build()
                .add(7, &[]);
            matching
                .u64_counter("requests_wrong_unit")
                .with_unit("s")
                .build()
                .add(9, &[]);
            matching
                .u64_gauge("requests_gauge")
                .with_unit("ms")
                .build()
                .record(11, &[]);
        });

        let renamed = find_metric(&metrics, "matching_meter", "renamed_requests");
        assert_eq!(renamed.description(), "renamed description");
        assert_eq!(renamed.unit(), "requests");
        match renamed.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                let point = sum.data_points().next().unwrap();
                assert_eq!(point.value(), 5);
                assert_eq!(
                    point.attributes().cloned().collect::<Vec<_>>(),
                    vec![KeyValue::new("route", "/items")]
                );
            }

            data => panic!("unexpected renamed stream data: {data:?}"),
        }
        assert_eq!(
            match find_metric(&metrics, "other_meter", "requests_total").data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    sum.data_points().next().unwrap().value()
                }
                data => panic!("unexpected non-matching data: {data:?}"),
            },
            7
        );
        assert_eq!(
            match find_metric(&metrics, "matching_meter", "requests_wrong_unit").data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    sum.data_points().next().unwrap().value()
                }
                data => panic!("unexpected wrong-unit data: {data:?}"),
            },
            9
        );
        match find_metric(&metrics, "matching_meter", "requests_gauge").data() {
            AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().value(), 11);
            }
            data => panic!("unexpected wrong-kind data: {data:?}"),
        }
    }

    #[test]
    fn scope_name_version_schema_and_attributes_all_select_views() {
        let mut view = default_config();
        view.name_pattern = Some("scoped_counter".to_string());
        view.meter_name = Some("scope.name".to_string());
        view.scope_version = Some("1.2.3".to_string());
        view.scope_schema_url = Some("https://example.test/schema".to_string());
        view.scope_attributes = vec![
            KeyValue::new("component", "checkout"),
            KeyValue::new("stable", true),
        ];
        view.output_name = Some("renamed_scoped_counter".to_string());

        let metrics = collect_with_views(vec![view], |provider| {
            let scopes = [
                InstrumentationScope::builder("scope.name")
                    .with_version("1.2.3")
                    .with_schema_url("https://example.test/schema")
                    .with_attributes([
                        KeyValue::new("component", "checkout"),
                        KeyValue::new("stable", true),
                        KeyValue::new("extra", 1_i64),
                    ])
                    .build(),
                InstrumentationScope::builder("scope.name")
                    .with_version("9.9.9")
                    .with_schema_url("https://example.test/schema")
                    .with_attributes([
                        KeyValue::new("component", "checkout"),
                        KeyValue::new("stable", true),
                    ])
                    .build(),
                InstrumentationScope::builder("scope.name")
                    .with_version("1.2.3")
                    .with_schema_url("https://example.test/other")
                    .with_attributes([
                        KeyValue::new("component", "checkout"),
                        KeyValue::new("stable", true),
                    ])
                    .build(),
                InstrumentationScope::builder("scope.name")
                    .with_version("1.2.3")
                    .with_schema_url("https://example.test/schema")
                    .with_attributes([
                        KeyValue::new("component", "shipping"),
                        KeyValue::new("stable", true),
                    ])
                    .build(),
            ];
            for scope in scopes {
                provider
                    .meter_with_scope(scope)
                    .u64_counter("scoped_counter")
                    .build()
                    .add(1, &[]);
            }
        });

        let names = metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .map(|metric| metric.name())
            .collect::<Vec<_>>();
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "renamed_scoped_counter")
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "scoped_counter")
                .count(),
            3
        );
    }

    #[test]
    fn empty_allow_list_and_cardinality_overflow_are_exported() {
        let mut empty_filter = default_config();
        empty_filter.name_pattern = Some("drop_attributes".to_string());
        empty_filter.attribute_filter_enabled = true;

        let mut cardinality = default_config();
        cardinality.name_pattern = Some("limited_counter".to_string());
        cardinality.cardinality_limit = Some(2);

        let metrics = collect_with_views(vec![empty_filter, cardinality], |provider| {
            let meter = provider.meter("views");
            meter.u64_gauge("drop_attributes").build().record(
                3,
                &[
                    KeyValue::new("route", "/items"),
                    KeyValue::new("status", 200_i64),
                ],
            );
            let counter = meter.u64_counter("limited_counter").build();
            counter.add(1, &[KeyValue::new("id", 1_i64)]);
            counter.add(2, &[KeyValue::new("id", 2_i64)]);
            counter.add(3, &[KeyValue::new("id", 3_i64)]);
        });

        match find_metric(&metrics, "views", "drop_attributes").data() {
            AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().attributes().count(), 0);
            }
            data => panic!("unexpected filtered gauge data: {data:?}"),
        }
        match find_metric(&metrics, "views", "limited_counter").data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                let points: Vec<_> = sum.data_points().collect();
                assert_eq!(points.len(), 3);
                let overflow = points
                    .iter()
                    .find(|point| {
                        point.attributes().any(|attribute| {
                            attribute.key.as_str() == "otel.metric.overflow"
                                && attribute.value == Value::Bool(true)
                        })
                    })
                    .expect("overflow series");
                assert_eq!(overflow.value(), 3);
            }
            data => panic!("unexpected limited counter data: {data:?}"),
        }
    }

    #[test]
    fn aggregation_variants_produce_expected_data() {
        let mut drop_view = default_config();
        drop_view.name_pattern = Some("dropped_counter".to_string());
        drop_view.aggregation = AggregationConfig::Drop;

        let mut sum_view = default_config();
        sum_view.name_pattern = Some("summed_up_down".to_string());
        sum_view.aggregation = AggregationConfig::Sum;

        let mut last_value_view = default_config();
        last_value_view.name_pattern = Some("last_value_gauge".to_string());
        last_value_view.aggregation = AggregationConfig::LastValue;

        let mut explicit_view = default_config();
        explicit_view.name_pattern = Some("explicit_histogram".to_string());
        explicit_view.aggregation = AggregationConfig::ExplicitHistogram {
            boundaries: vec![1.0, 5.0, 10.0],
            record_min_max: true,
        };

        let mut exponential_view = default_config();
        exponential_view.name_pattern = Some("exponential_histogram".to_string());
        exponential_view.aggregation = AggregationConfig::ExponentialHistogram {
            max_size: 8,
            max_scale: 4,
            record_min_max: true,
        };

        let metrics = collect_with_views(
            vec![
                drop_view,
                sum_view,
                last_value_view,
                explicit_view,
                exponential_view,
            ],
            |provider| {
                let meter = provider.meter("aggregations");
                meter.u64_counter("dropped_counter").build().add(1, &[]);
                let up_down = meter.i64_up_down_counter("summed_up_down").build();
                up_down.add(8, &[]);
                up_down.add(-3, &[]);
                let gauge = meter.f64_gauge("last_value_gauge").build();
                gauge.record(9.0, &[]);
                gauge.record(2.5, &[]);
                let explicit = meter.f64_histogram("explicit_histogram").build();
                explicit.record(2.0, &[]);
                explicit.record(7.0, &[]);
                let exponential = meter.f64_histogram("exponential_histogram").build();
                exponential.record(2.0, &[]);
                exponential.record(8.0, &[]);
            },
        );

        assert!(metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .all(|metric| metric.name() != "dropped_counter"));
        match find_metric(&metrics, "aggregations", "summed_up_down").data() {
            AggregatedMetrics::I64(MetricData::Sum(sum)) => {
                assert_eq!(sum.data_points().next().unwrap().value(), 5);
            }
            data => panic!("unexpected sum data: {data:?}"),
        }
        match find_metric(&metrics, "aggregations", "last_value_gauge").data() {
            AggregatedMetrics::F64(MetricData::Gauge(gauge)) => {
                assert_eq!(gauge.data_points().next().unwrap().value(), 2.5);
            }
            data => panic!("unexpected last-value data: {data:?}"),
        }
        match find_metric(&metrics, "aggregations", "explicit_histogram").data() {
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                let point = histogram.data_points().next().unwrap();
                assert_eq!(point.count(), 2);
                assert_eq!(point.sum(), 9.0);
                assert_eq!(point.min(), Some(2.0));
                assert_eq!(point.max(), Some(7.0));
                assert_eq!(point.bounds().collect::<Vec<_>>(), [1.0, 5.0, 10.0]);
                assert_eq!(point.bucket_counts().collect::<Vec<_>>(), [0, 1, 1, 0]);
            }
            data => panic!("unexpected explicit histogram data: {data:?}"),
        }
        match find_metric(&metrics, "aggregations", "exponential_histogram").data() {
            AggregatedMetrics::F64(MetricData::ExponentialHistogram(histogram)) => {
                let point = histogram.data_points().next().unwrap();
                assert_eq!(point.count(), 2);
                assert_eq!(point.sum(), 10.0);
                assert_eq!(point.min(), Some(2.0));
                assert_eq!(point.max(), Some(8.0));
                assert!(point.scale() <= 4);
                assert!(point.positive_bucket().counts().sum::<u64>() + point.zero_count() == 2);
            }
            data => panic!("unexpected exponential histogram data: {data:?}"),
        }
    }

    #[test]
    fn view_validation_rejects_invalid_histogram_configuration() {
        unsafe {
            let builder = otel_metric_view_builder_new();
            let invalid_order = [2.0, 1.0];
            assert_eq!(
                otel_metric_view_builder_set_explicit_histogram(
                    builder,
                    invalid_order.as_ptr(),
                    invalid_order.len(),
                    1,
                ),
                OtelStatus::InvalidConfig
            );
            assert!(crate::api_ffi::test_probe::last_error().contains("strictly increasing"));

            let invalid_nan = [1.0, f64::NAN];
            assert_eq!(
                otel_metric_view_builder_set_explicit_histogram(
                    builder,
                    invalid_nan.as_ptr(),
                    invalid_nan.len(),
                    1,
                ),
                OtelStatus::InvalidConfig
            );
            let invalid_infinity = [1.0, f64::INFINITY];
            assert_eq!(
                otel_metric_view_builder_set_explicit_histogram(
                    builder,
                    invalid_infinity.as_ptr(),
                    invalid_infinity.len(),
                    1,
                ),
                OtelStatus::InvalidConfig
            );
            assert_eq!(
                otel_metric_view_builder_set_exponential_histogram(builder, 0, 4, 1),
                OtelStatus::InvalidConfig
            );
            assert_eq!(
                otel_metric_view_builder_set_exponential_histogram(builder, 8, 21, 1),
                OtelStatus::InvalidConfig
            );
            assert_eq!(
                otel_metric_view_builder_set_exponential_histogram(builder, 8, -11, 1),
                OtelStatus::InvalidConfig
            );
            otel_metric_view_builder_destroy(builder);
        }
    }

    #[test]
    fn scope_matcher_builder_copies_fields_and_rejects_duplicate_keys() {
        unsafe {
            let builder = otel_metric_view_builder_new();
            let version = b"1.2.3";
            let schema = b"https://example.test/schema";
            assert_eq!(
                otel_metric_view_builder_set_scope_version(
                    builder,
                    OtelStringView {
                        ptr: version.as_ptr().cast(),
                        len: version.len(),
                    },
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_metric_view_builder_set_scope_schema_url(
                    builder,
                    OtelStringView {
                        ptr: schema.as_ptr().cast(),
                        len: schema.len(),
                    },
                ),
                OtelStatus::Ok
            );
            let key = b"component";
            let value = b"checkout";
            let attribute = OtelKeyValue {
                key: OtelStringView {
                    ptr: key.as_ptr().cast(),
                    len: key.len(),
                },
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: OtelStringView {
                        ptr: value.as_ptr().cast(),
                        len: value.len(),
                    },
                },
            };
            assert_eq!(
                otel_metric_view_builder_add_scope_attribute(builder, &attribute),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_metric_view_builder_add_scope_attribute(builder, &attribute),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_metric_view_builder_add_scope_attribute(builder, std::ptr::null()),
                OtelStatus::InvalidArgument
            );

            let mut view = std::ptr::null_mut();
            assert_eq!(
                otel_metric_view_builder_build(builder, &mut view),
                OtelStatus::Ok
            );
            assert_eq!((*view).config.scope_version.as_deref(), Some("1.2.3"));
            assert_eq!(
                (*view).config.scope_schema_url.as_deref(),
                Some("https://example.test/schema")
            );
            assert_eq!(
                (*view).config.scope_attributes,
                vec![KeyValue::new("component", "checkout")]
            );
            otel_metric_view_destroy(view);
            otel_metric_view_builder_destroy(builder);
        }
    }

    #[test]
    fn view_builder_enforces_scope_and_allowed_attribute_limits() {
        unsafe {
            let builder = otel_metric_view_builder_new();
            (*builder).config.scope_attributes =
                vec![KeyValue::new("existing", "value"); MAX_VIEW_SCOPE_ATTRIBUTES];
            let key = b"extra";
            let value = b"value";
            let attribute = OtelKeyValue {
                key: OtelStringView {
                    ptr: key.as_ptr().cast(),
                    len: key.len(),
                },
                value_type: OtelAttributeType::String as u32,
                value: OtelAttributeValue {
                    string_value: OtelStringView {
                        ptr: value.as_ptr().cast(),
                        len: value.len(),
                    },
                },
            };
            assert_eq!(
                otel_metric_view_builder_add_scope_attribute(builder, &attribute),
                OtelStatus::InvalidConfig
            );

            (*builder).config.allowed_attributes =
                vec!["existing".to_owned(); MAX_VIEW_ALLOWED_ATTRIBUTES];
            assert_eq!(
                otel_metric_view_builder_add_allowed_attribute(
                    builder,
                    OtelStringView {
                        ptr: key.as_ptr().cast(),
                        len: key.len(),
                    },
                ),
                OtelStatus::InvalidConfig
            );
            otel_metric_view_builder_destroy(builder);
        }
    }

    #[test]
    fn wildcard_matching_supports_documented_single_star_patterns() {
        assert!(wildcard_matches("*", "anything"));
        assert!(wildcard_matches("requests_*", "requests_total"));
        assert!(wildcard_matches("*_duration", "http_duration"));
        assert!(wildcard_matches("http_*_seconds", "http_server_seconds"));
        assert!(wildcard_matches("exact", "exact"));
        assert!(!wildcard_matches("requests_*", "http_requests"));
        assert!(!wildcard_matches("exact", "exact_suffix"));
    }

    #[test]
    fn view_build_rejects_multiple_wildcards_without_consuming_builder() {
        unsafe {
            let builder = otel_metric_view_builder_new();
            let invalid = b"http_*_*";
            assert_eq!(
                otel_metric_view_builder_set_name_pattern(
                    builder,
                    OtelStringView {
                        ptr: invalid.as_ptr().cast(),
                        len: invalid.len(),
                    },
                ),
                OtelStatus::Ok
            );
            let mut view = std::ptr::NonNull::<OtelMetricView>::dangling().as_ptr();
            assert_eq!(
                otel_metric_view_builder_build(builder, &mut view),
                OtelStatus::InvalidConfig
            );
            assert!(view.is_null());
            assert!(crate::api_ffi::test_probe::last_error().contains("at most one '*'"));

            let valid = b"http_*";
            assert_eq!(
                otel_metric_view_builder_set_name_pattern(
                    builder,
                    OtelStringView {
                        ptr: valid.as_ptr().cast(),
                        len: valid.len(),
                    },
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_metric_view_builder_build(builder, &mut view),
                OtelStatus::Ok
            );
            assert!(!view.is_null());
            otel_metric_view_destroy(view);
            otel_metric_view_builder_destroy(builder);
        }
    }
}
