//! Callback-scoped traversal of aggregated Metrics export data.

use std::cell::RefCell;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry::{Array, KeyValue, Value};
use opentelemetry_c_abi::{OtelAttributeValue, OtelBool, OtelStatus, OtelStringView};
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, Exemplar, ExponentialHistogram, Gauge, Histogram, Metric, MetricData,
    ResourceMetrics, Sum,
};
use opentelemetry_sdk::metrics::Temporality;

use crate::error::{clear_last_error, fail, fail_owned};
use crate::handle::guard_status;

pub const OTEL_METRIC_NUMBER_U64: u32 = 0;
pub const OTEL_METRIC_NUMBER_I64: u32 = 1;
pub const OTEL_METRIC_NUMBER_F64: u32 = 2;

pub const OTEL_METRIC_DATA_GAUGE: u32 = 0;
pub const OTEL_METRIC_DATA_SUM: u32 = 1;
pub const OTEL_METRIC_DATA_HISTOGRAM: u32 = 2;
pub const OTEL_METRIC_DATA_EXPONENTIAL_HISTOGRAM: u32 = 3;

pub const OTEL_METRIC_TEMPORALITY_CUMULATIVE: u32 = 1;
pub const OTEL_METRIC_TEMPORALITY_DELTA: u32 = 2;
pub const OTEL_METRIC_TEMPORALITY_LOW_MEMORY: u32 = 3;

pub const OTEL_METRIC_ATTRIBUTE_STRING_ARRAY: u32 = 4;
pub const OTEL_METRIC_ATTRIBUTE_BOOL_ARRAY: u32 = 5;
pub const OTEL_METRIC_ATTRIBUTE_INT64_ARRAY: u32 = 6;
pub const OTEL_METRIC_ATTRIBUTE_DOUBLE_ARRAY: u32 = 7;

#[repr(C)]
pub struct OtelMetricBatch {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricArrayView {
    pub values: *const c_void,
    pub count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OtelMetricAttributeValue {
    pub scalar: OtelAttributeValue,
    pub array: OtelMetricArrayView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricAttribute {
    pub key: OtelStringView,
    pub value_type: u32,
    pub value: OtelMetricAttributeValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OtelMetricNumber {
    pub u64_value: u64,
    pub i64_value: i64,
    pub f64_value: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricMetadata {
    pub name: OtelStringView,
    pub description: OtelStringView,
    pub unit: OtelStringView,
    pub data_kind: u32,
    pub number_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricPoint {
    pub point_index: usize,
    pub start_time_unix_nanos: u64,
    pub time_unix_nanos: u64,
    pub temporality: u32,
    pub is_monotonic: OtelBool,
    pub value: OtelMetricNumber,
    pub count: u64,
    pub sum: OtelMetricNumber,
    pub min: OtelMetricNumber,
    pub max: OtelMetricNumber,
    pub has_min: OtelBool,
    pub has_max: OtelBool,
    pub scale: i8,
    pub _padding: [u8; 7],
    pub zero_count: u64,
    pub zero_threshold: f64,
    pub positive_bucket_offset: i32,
    pub negative_bucket_offset: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricExemplar {
    pub point_index: usize,
    pub exemplar_index: usize,
    pub time_unix_nanos: u64,
    pub value: OtelMetricNumber,
    pub span_id: [u8; 8],
    pub trace_id: [u8; 16],
}

pub type OtelMetricVisitResource =
    Option<extern "C" fn(*mut c_void, *const OtelMetricAttribute, usize) -> OtelStatus>;
pub type OtelMetricVisitScope = Option<
    extern "C" fn(
        *mut c_void,
        OtelStringView,
        OtelStringView,
        OtelStringView,
        *const OtelMetricAttribute,
        usize,
    ) -> OtelStatus,
>;
pub type OtelMetricVisitMetric =
    Option<extern "C" fn(*mut c_void, *const OtelMetricMetadata) -> OtelStatus>;
pub type OtelMetricVisitPoint = Option<
    extern "C" fn(
        *mut c_void,
        *const OtelMetricPoint,
        *const OtelMetricAttribute,
        usize,
        *const f64,
        usize,
        *const u64,
        usize,
        *const u64,
        usize,
        *const u64,
        usize,
    ) -> OtelStatus,
>;
pub type OtelMetricVisitExemplar = Option<
    extern "C" fn(
        *mut c_void,
        *const OtelMetricExemplar,
        *const OtelMetricAttribute,
        usize,
    ) -> OtelStatus,
>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricVisitor {
    pub struct_size: usize,
    pub resource: OtelMetricVisitResource,
    pub scope: OtelMetricVisitScope,
    pub metric: OtelMetricVisitMetric,
    pub point: OtelMetricVisitPoint,
    pub exemplar: OtelMetricVisitExemplar,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelMetricArrayView>() == 16);
    assert!(std::mem::size_of::<OtelMetricAttribute>() == 40);
    assert!(std::mem::size_of::<OtelMetricMetadata>() == 56);
    assert!(std::mem::size_of::<OtelMetricPoint>() == 112);
    assert!(std::mem::size_of::<OtelMetricExemplar>() == 56);
    assert!(std::mem::size_of::<OtelMetricVisitor>() == 48);
};

static NEXT_BATCH_TOKEN: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    static ACTIVE_BATCHES: RefCell<Vec<(usize, *const ResourceMetrics)>> =
        const { RefCell::new(Vec::new()) };
}

pub(crate) struct MetricBatchRegistration {
    token: usize,
}

impl MetricBatchRegistration {
    pub(crate) fn new(metrics: &ResourceMetrics) -> Self {
        let token = loop {
            let token = NEXT_BATCH_TOKEN.fetch_add(1, Ordering::Relaxed);
            if token != 0 {
                break token;
            }
        };
        ACTIVE_BATCHES.with(|batches| {
            batches
                .borrow_mut()
                .push((token, metrics as *const ResourceMetrics));
        });
        Self { token }
    }

    pub(crate) fn token(&self) -> *const OtelMetricBatch {
        self.token as *const OtelMetricBatch
    }
}

impl Drop for MetricBatchRegistration {
    fn drop(&mut self) {
        ACTIVE_BATCHES.with(|batches| {
            let mut batches = batches.borrow_mut();
            if let Some(index) = batches.iter().rposition(|(token, _)| *token == self.token) {
                batches.remove(index);
            }
        });
    }
}

fn active_batch(token: usize) -> Option<*const ResourceMetrics> {
    ACTIVE_BATCHES.with(|batches| {
        batches
            .borrow()
            .iter()
            .rev()
            .find_map(|(candidate, metrics)| (*candidate == token).then_some(*metrics))
    })
}

fn string_view(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast(),
        len: value.len(),
    }
}

enum AttributeBacking<'a> {
    Scalar(OtelAttributeValue),
    BoolArray(Vec<OtelBool>),
    I64Array(&'a [i64]),
    F64Array(&'a [f64]),
    StringArray(Vec<OtelStringView>),
}

struct ConvertedAttribute<'a> {
    key: &'a str,
    value_type: u32,
    backing: AttributeBacking<'a>,
}

fn convert_attribute_parts<'a>(key: &'a str, value: &'a Value) -> ConvertedAttribute<'a> {
    let (value_type, backing) = match value {
        Value::String(value) => (
            0,
            AttributeBacking::Scalar(OtelAttributeValue {
                string_value: string_view(value.as_str()),
            }),
        ),
        Value::Bool(value) => (
            1,
            AttributeBacking::Scalar(OtelAttributeValue {
                bool_value: u32::from(*value),
            }),
        ),
        Value::I64(value) => (
            2,
            AttributeBacking::Scalar(OtelAttributeValue {
                int64_value: *value,
            }),
        ),
        Value::F64(value) => (
            3,
            AttributeBacking::Scalar(OtelAttributeValue {
                double_value: *value,
            }),
        ),
        Value::Array(Array::String(values)) => (
            OTEL_METRIC_ATTRIBUTE_STRING_ARRAY,
            AttributeBacking::StringArray(
                values
                    .iter()
                    .map(|value| string_view(value.as_str()))
                    .collect(),
            ),
        ),
        Value::Array(Array::Bool(values)) => (
            OTEL_METRIC_ATTRIBUTE_BOOL_ARRAY,
            AttributeBacking::BoolArray(values.iter().map(|value| u32::from(*value)).collect()),
        ),
        Value::Array(Array::I64(values)) => (
            OTEL_METRIC_ATTRIBUTE_INT64_ARRAY,
            AttributeBacking::I64Array(values),
        ),
        Value::Array(Array::F64(values)) => (
            OTEL_METRIC_ATTRIBUTE_DOUBLE_ARRAY,
            AttributeBacking::F64Array(values),
        ),
        _ => unreachable!("all OpenTelemetry attribute variants are handled"),
    };
    ConvertedAttribute {
        key,
        value_type,
        backing,
    }
}

fn convert_attribute(attribute: &KeyValue) -> ConvertedAttribute<'_> {
    convert_attribute_parts(attribute.key.as_str(), &attribute.value)
}

fn call_with_converted_attributes(
    converted: Vec<ConvertedAttribute<'_>>,
    call: impl FnOnce(*const OtelMetricAttribute, usize) -> OtelStatus,
) -> OtelStatus {
    let views: Vec<_> = converted
        .iter()
        .map(|attribute| {
            let value = match &attribute.backing {
                AttributeBacking::Scalar(value) => OtelMetricAttributeValue { scalar: *value },
                AttributeBacking::BoolArray(values) => OtelMetricAttributeValue {
                    array: OtelMetricArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
                AttributeBacking::I64Array(values) => OtelMetricAttributeValue {
                    array: OtelMetricArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
                AttributeBacking::F64Array(values) => OtelMetricAttributeValue {
                    array: OtelMetricArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
                AttributeBacking::StringArray(values) => OtelMetricAttributeValue {
                    array: OtelMetricArrayView {
                        values: values.as_ptr().cast(),
                        count: values.len(),
                    },
                },
            };
            OtelMetricAttribute {
                key: string_view(attribute.key),
                value_type: attribute.value_type,
                value,
            }
        })
        .collect();
    call(views.as_ptr(), views.len())
}

fn with_attributes<'a>(
    attributes: impl Iterator<Item = &'a KeyValue>,
    call: impl FnOnce(*const OtelMetricAttribute, usize) -> OtelStatus,
) -> OtelStatus {
    let converted: Vec<_> = attributes.map(convert_attribute).collect();
    call_with_converted_attributes(converted, call)
}

fn with_resource_attributes(
    metrics: &ResourceMetrics,
    call: impl FnOnce(*const OtelMetricAttribute, usize) -> OtelStatus,
) -> OtelStatus {
    let converted: Vec<_> = metrics
        .resource()
        .iter()
        .map(|(key, value)| convert_attribute_parts(key.as_str(), value))
        .collect();
    call_with_converted_attributes(converted, call)
}

fn timestamp(value: SystemTime) -> Result<u64, OtelStatus> {
    let nanos = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            fail(
                OtelStatus::InternalError,
                "metric timestamp predates Unix epoch",
            )
        })?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| {
        fail(
            OtelStatus::InternalError,
            "metric timestamp exceeds the C nanosecond range",
        )
    })
}

fn optional_timestamp(value: Option<SystemTime>) -> Result<u64, OtelStatus> {
    value
        .map(timestamp)
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn temporality(value: Temporality) -> u32 {
    match value {
        Temporality::Cumulative => OTEL_METRIC_TEMPORALITY_CUMULATIVE,
        Temporality::Delta => OTEL_METRIC_TEMPORALITY_DELTA,
        Temporality::LowMemory => OTEL_METRIC_TEMPORALITY_LOW_MEMORY,
        _ => OTEL_METRIC_TEMPORALITY_CUMULATIVE,
    }
}

trait Number: Copy {
    fn into_abi(self) -> OtelMetricNumber;
}

impl Number for u64 {
    fn into_abi(self) -> OtelMetricNumber {
        OtelMetricNumber { u64_value: self }
    }
}

impl Number for i64 {
    fn into_abi(self) -> OtelMetricNumber {
        OtelMetricNumber { i64_value: self }
    }
}

impl Number for f64 {
    fn into_abi(self) -> OtelMetricNumber {
        OtelMetricNumber { f64_value: self }
    }
}

fn zero_number() -> OtelMetricNumber {
    OtelMetricNumber { u64_value: 0 }
}

struct Visitor<'a> {
    callbacks: &'a OtelMetricVisitor,
    user_data: *mut c_void,
}

impl Visitor<'_> {
    fn check(&self, status: OtelStatus, operation: &str) -> Result<(), OtelStatus> {
        if status == OtelStatus::Ok {
            Ok(())
        } else {
            Err(fail_owned(
                status,
                format!("custom metric visitor {operation} callback failed"),
            ))
        }
    }

    fn visit_exemplars<'a, T: Number + 'a>(
        &self,
        point_index: usize,
        exemplars: impl Iterator<Item = &'a Exemplar<T>>,
    ) -> Result<(), OtelStatus> {
        let Some(callback) = self.callbacks.exemplar else {
            return Ok(());
        };
        for (exemplar_index, exemplar) in exemplars.enumerate() {
            let summary = OtelMetricExemplar {
                point_index,
                exemplar_index,
                time_unix_nanos: timestamp(exemplar.time())?,
                value: exemplar.value.into_abi(),
                span_id: *exemplar.span_id(),
                trace_id: *exemplar.trace_id(),
            };
            let status = with_attributes(exemplar.filtered_attributes(), |attributes, count| {
                callback(self.user_data, &summary, attributes, count)
            });
            self.check(status, "exemplar")?;
        }
        Ok(())
    }

    fn visit_gauge<T: Number>(&self, gauge: &Gauge<T>) -> Result<(), OtelStatus> {
        for (point_index, point) in gauge.data_points().enumerate() {
            let summary = OtelMetricPoint {
                point_index,
                start_time_unix_nanos: optional_timestamp(gauge.start_time())?,
                time_unix_nanos: timestamp(gauge.time())?,
                temporality: 0,
                is_monotonic: 0,
                value: point.value().into_abi(),
                count: 0,
                sum: zero_number(),
                min: zero_number(),
                max: zero_number(),
                has_min: 0,
                has_max: 0,
                scale: 0,
                _padding: [0; 7],
                zero_count: 0,
                zero_threshold: 0.0,
                positive_bucket_offset: 0,
                negative_bucket_offset: 0,
            };
            if let Some(callback) = self.callbacks.point {
                let status = with_attributes(point.attributes(), |attributes, count| {
                    callback(
                        self.user_data,
                        &summary,
                        attributes,
                        count,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                    )
                });
                self.check(status, "point")?;
            }
            self.visit_exemplars(point_index, point.exemplars())?;
        }
        Ok(())
    }

    fn visit_sum<T: Number>(&self, sum: &Sum<T>) -> Result<(), OtelStatus> {
        for (point_index, point) in sum.data_points().enumerate() {
            let summary = OtelMetricPoint {
                point_index,
                start_time_unix_nanos: timestamp(sum.start_time())?,
                time_unix_nanos: timestamp(sum.time())?,
                temporality: temporality(sum.temporality()),
                is_monotonic: u32::from(sum.is_monotonic()),
                value: point.value().into_abi(),
                count: 0,
                sum: zero_number(),
                min: zero_number(),
                max: zero_number(),
                has_min: 0,
                has_max: 0,
                scale: 0,
                _padding: [0; 7],
                zero_count: 0,
                zero_threshold: 0.0,
                positive_bucket_offset: 0,
                negative_bucket_offset: 0,
            };
            if let Some(callback) = self.callbacks.point {
                let status = with_attributes(point.attributes(), |attributes, count| {
                    callback(
                        self.user_data,
                        &summary,
                        attributes,
                        count,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                    )
                });
                self.check(status, "point")?;
            }
            self.visit_exemplars(point_index, point.exemplars())?;
        }
        Ok(())
    }

    fn visit_histogram<T: Number>(&self, histogram: &Histogram<T>) -> Result<(), OtelStatus> {
        for (point_index, point) in histogram.data_points().enumerate() {
            let bounds: Vec<_> = point.bounds().collect();
            let bucket_counts: Vec<_> = point.bucket_counts().collect();
            let summary = OtelMetricPoint {
                point_index,
                start_time_unix_nanos: timestamp(histogram.start_time())?,
                time_unix_nanos: timestamp(histogram.time())?,
                temporality: temporality(histogram.temporality()),
                is_monotonic: 0,
                value: zero_number(),
                count: point.count(),
                sum: point.sum().into_abi(),
                min: point
                    .min()
                    .map(Number::into_abi)
                    .unwrap_or_else(zero_number),
                max: point
                    .max()
                    .map(Number::into_abi)
                    .unwrap_or_else(zero_number),
                has_min: u32::from(point.min().is_some()),
                has_max: u32::from(point.max().is_some()),
                scale: 0,
                _padding: [0; 7],
                zero_count: 0,
                zero_threshold: 0.0,
                positive_bucket_offset: 0,
                negative_bucket_offset: 0,
            };
            if let Some(callback) = self.callbacks.point {
                let status = with_attributes(point.attributes(), |attributes, count| {
                    callback(
                        self.user_data,
                        &summary,
                        attributes,
                        count,
                        bounds.as_ptr(),
                        bounds.len(),
                        bucket_counts.as_ptr(),
                        bucket_counts.len(),
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                    )
                });
                self.check(status, "point")?;
            }
            self.visit_exemplars(point_index, point.exemplars())?;
        }
        Ok(())
    }

    fn visit_exponential_histogram<T: Number>(
        &self,
        histogram: &ExponentialHistogram<T>,
    ) -> Result<(), OtelStatus> {
        for (point_index, point) in histogram.data_points().enumerate() {
            let positive: Vec<_> = point.positive_bucket().counts().collect();
            let negative: Vec<_> = point.negative_bucket().counts().collect();
            let summary = OtelMetricPoint {
                point_index,
                start_time_unix_nanos: timestamp(histogram.start_time())?,
                time_unix_nanos: timestamp(histogram.time())?,
                temporality: temporality(histogram.temporality()),
                is_monotonic: 0,
                value: zero_number(),
                count: u64::try_from(point.count()).unwrap_or(u64::MAX),
                sum: point.sum().into_abi(),
                min: point
                    .min()
                    .map(Number::into_abi)
                    .unwrap_or_else(zero_number),
                max: point
                    .max()
                    .map(Number::into_abi)
                    .unwrap_or_else(zero_number),
                has_min: u32::from(point.min().is_some()),
                has_max: u32::from(point.max().is_some()),
                scale: point.scale(),
                _padding: [0; 7],
                zero_count: point.zero_count(),
                zero_threshold: point.zero_threshold(),
                positive_bucket_offset: point.positive_bucket().offset(),
                negative_bucket_offset: point.negative_bucket().offset(),
            };
            if let Some(callback) = self.callbacks.point {
                let status = with_attributes(point.attributes(), |attributes, count| {
                    callback(
                        self.user_data,
                        &summary,
                        attributes,
                        count,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        0,
                        positive.as_ptr(),
                        positive.len(),
                        negative.as_ptr(),
                        negative.len(),
                    )
                });
                self.check(status, "point")?;
            }
            self.visit_exemplars(point_index, point.exemplars())?;
        }
        Ok(())
    }

    fn visit_metric(&self, metric: &Metric) -> Result<(), OtelStatus> {
        macro_rules! visit_data {
            ($number:expr, $data:expr) => {{
                match $data {
                    MetricData::Gauge(data) => {
                        self.announce_metric(metric, OTEL_METRIC_DATA_GAUGE, $number)?;
                        self.visit_gauge(data)
                    }
                    MetricData::Sum(data) => {
                        self.announce_metric(metric, OTEL_METRIC_DATA_SUM, $number)?;
                        self.visit_sum(data)
                    }
                    MetricData::Histogram(data) => {
                        self.announce_metric(metric, OTEL_METRIC_DATA_HISTOGRAM, $number)?;
                        self.visit_histogram(data)
                    }
                    MetricData::ExponentialHistogram(data) => {
                        self.announce_metric(
                            metric,
                            OTEL_METRIC_DATA_EXPONENTIAL_HISTOGRAM,
                            $number,
                        )?;
                        self.visit_exponential_histogram(data)
                    }
                }
            }};
        }

        match metric.data() {
            AggregatedMetrics::U64(data) => visit_data!(OTEL_METRIC_NUMBER_U64, data),
            AggregatedMetrics::I64(data) => visit_data!(OTEL_METRIC_NUMBER_I64, data),
            AggregatedMetrics::F64(data) => visit_data!(OTEL_METRIC_NUMBER_F64, data),
        }
    }

    fn announce_metric(
        &self,
        metric: &Metric,
        data_kind: u32,
        number_kind: u32,
    ) -> Result<(), OtelStatus> {
        if let Some(callback) = self.callbacks.metric {
            let metadata = OtelMetricMetadata {
                name: string_view(metric.name()),
                description: string_view(metric.description()),
                unit: string_view(metric.unit()),
                data_kind,
                number_kind,
            };
            self.check(callback(self.user_data, &metadata), "metric")?;
        }
        Ok(())
    }
}

/// Traverse one callback-scoped Metrics batch.
///
/// # Safety
///
/// `batch` must be the token passed to the currently executing custom exporter callback on
/// this thread. `visitor` must address a readable complete visitor structure.
#[no_mangle]
pub unsafe extern "C" fn otel_metric_batch_visit(
    batch: *const OtelMetricBatch,
    visitor: *const OtelMetricVisitor,
    user_data: *mut c_void,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let metrics = match active_batch(batch as usize) {
            Some(metrics) => unsafe { &*metrics },
            None => {
                return fail(
                    OtelStatus::InvalidArgument,
                    "metric batch is not active for this exporter callback thread",
                )
            }
        };
        if visitor.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "metric visitor must not be NULL",
            );
        }
        let struct_size = unsafe { std::ptr::read_unaligned(visitor.cast::<usize>()) };
        if struct_size < std::mem::size_of::<OtelMetricVisitor>() {
            return fail(
                OtelStatus::InvalidConfig,
                "metric visitor structure is smaller than the required ABI size",
            );
        }
        let visitor = unsafe { &*visitor };
        let traversal = Visitor {
            callbacks: visitor,
            user_data,
        };

        if let Some(callback) = visitor.resource {
            let status = with_resource_attributes(metrics, |attributes, count| {
                callback(user_data, attributes, count)
            });
            if let Err(status) = traversal.check(status, "resource") {
                return status;
            }
        }
        for scope in metrics.scope_metrics() {
            if let Some(callback) = visitor.scope {
                let version = scope.scope().version().unwrap_or_default();
                let schema_url = scope.scope().schema_url().unwrap_or_default();
                let status = with_attributes(scope.scope().attributes(), |attributes, count| {
                    callback(
                        user_data,
                        string_view(scope.scope().name()),
                        string_view(version),
                        string_view(schema_url),
                        attributes,
                        count,
                    )
                });
                if let Err(status) = traversal.check(status, "scope") {
                    return status;
                }
            }
            for metric in scope.metrics() {
                if let Err(status) = traversal.visit_metric(metric) {
                    return status;
                }
            }
        }
        OtelStatus::Ok
    })
}
