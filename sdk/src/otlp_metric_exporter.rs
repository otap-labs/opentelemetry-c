//! OTLP HTTP/protobuf Metrics exporter builder.

use std::time::Duration;

use opentelemetry_c_abi::{OtelStatus, OtelStringView};

use crate::error::{clear_last_error, fail, fail_abi, fail_owned};
use crate::handle::{
    checked_mut, checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasMagic,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

#[cfg(feature = "otlp")]
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig, WithHttpConfig};
#[cfg(feature = "otlp")]
use opentelemetry_sdk::metrics::Temporality;
#[cfg(feature = "otlp")]
use std::collections::HashMap;

const BUILDER_MAGIC: u64 = 0x4F54_4C43_4D4F_544C;

#[derive(Default)]
struct Config {
    endpoint: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Option<Duration>,
    temporality: u32,
}

pub struct OtelOtlpMetricExporterBuilder {
    magic: u64,
    config: Config,
}

impl HasMagic for OtelOtlpMetricExporterBuilder {
    const MAGIC: u64 = BUILDER_MAGIC;
    fn magic(&self) -> u64 {
        self.magic
    }
    fn set_magic(&mut self, value: u64) {
        self.magic = value;
    }
}

#[no_mangle]
pub extern "C" fn otel_otlp_metric_exporter_builder_new() -> *mut OtelOtlpMetricExporterBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelOtlpMetricExporterBuilder {
            magic: BUILDER_MAGIC,
            config: Config::default(),
        })
    })
}

/// Destroy an OTLP Metrics exporter builder.
///
/// # Safety
///
/// `builder` must be NULL or a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_destroy(
    builder: *mut OtelOtlpMetricExporterBuilder,
) {
    guard_unit(|| unsafe { destroy(builder) });
}

unsafe fn with_config(
    builder: *mut OtelOtlpMetricExporterBuilder,
    f: impl FnOnce(&mut Config) -> OtelStatus,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        match unsafe { checked_mut(builder) } {
            Some(builder) => f(&mut builder.config),
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Set the OTLP Metrics endpoint.
///
/// # Safety
///
/// `builder` must be live and `endpoint` must address readable bytes for the call.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_set_endpoint(
    builder: *mut OtelOtlpMetricExporterBuilder,
    endpoint: OtelStringView,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| match endpoint.to_string_strict() {
            Ok(endpoint) => {
                config.endpoint = Some(endpoint);
                OtelStatus::Ok
            }
            Err(err) => fail_abi(err),
        })
    }
}

/// Add an OTLP request header.
///
/// # Safety
///
/// `builder` must be live and both string views must address readable bytes for the call.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_add_header(
    builder: *mut OtelOtlpMetricExporterBuilder,
    key: OtelStringView,
    value: OtelStringView,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            let key = match key.to_string_strict() {
                Ok(key) if !key.is_empty() => key,
                Ok(_) => {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "OTLP metric header key must not be empty",
                    )
                }
                Err(err) => return fail_abi(err),
            };
            if config
                .headers
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(&key))
            {
                return fail_owned(
                    OtelStatus::InvalidArgument,
                    format!("OTLP metric header key already exists: {key}"),
                );
            }
            let value = match value.to_string_strict() {
                Ok(value) => value,
                Err(err) => return fail_abi(err),
            };
            config.headers.push((key, value));
            OtelStatus::Ok
        })
    }
}

/// Set the exporter timeout.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_set_timeout_millis(
    builder: *mut OtelOtlpMetricExporterBuilder,
    timeout_millis: u64,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.timeout = (timeout_millis != 0).then(|| Duration::from_millis(timeout_millis));
            OtelStatus::Ok
        })
    }
}

/// Temporality values: 0=environment/default, 1=cumulative, 2=delta, 3=low-memory.
/// Set the exporter temporality preference.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_set_temporality(
    builder: *mut OtelOtlpMetricExporterBuilder,
    temporality: u32,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            if temporality > 3 {
                return fail(
                    OtelStatus::InvalidArgument,
                    "unknown metric temporality value",
                );
            }
            config.temporality = temporality;
            OtelStatus::Ok
        })
    }
}

#[cfg(feature = "otlp")]
fn build_exporter(config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    let mut builder = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary);
    if let Some(endpoint) = &config.endpoint {
        builder = builder.with_endpoint(endpoint.clone());
    }
    if let Some(timeout) = config.timeout {
        builder = builder.with_timeout(timeout);
    }
    if !config.headers.is_empty() {
        let headers: HashMap<String, String> = config.headers.iter().cloned().collect();
        builder = builder.with_headers(headers);
    }
    builder = match config.temporality {
        0 => builder,
        1 => builder.with_temporality(Temporality::Cumulative),
        2 => builder.with_temporality(Temporality::Delta),
        3 => builder.with_temporality(Temporality::LowMemory),
        _ => unreachable!(),
    };
    builder
        .build()
        .map(MetricExporterImpl::Otlp)
        .map_err(|err| {
            fail_owned(
                OtelStatus::InvalidConfig,
                format!("failed to build OTLP metric exporter: {err}"),
            )
        })
}

#[cfg(not(feature = "otlp"))]
fn build_exporter(_config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    Err(fail(
        OtelStatus::InvalidConfig,
        "OTLP metric exporter is unavailable: rebuild with the `otlp` feature",
    ))
}

/// Build an owned OTLP Metrics exporter.
///
/// # Safety
///
/// `builder` must be live and `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_build(
    builder: *const OtelOtlpMetricExporterBuilder,
    out: *mut *mut OtelMetricExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let builder = match unsafe { checked_ref(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        let exporter = match build_exporter(&builder.config) {
            Ok(exporter) => exporter,
            Err(status) => return status,
        };
        unsafe { *out = into_raw(OtelMetricExporter::new(exporter)) };
        OtelStatus::Ok
    })
}
