//! OTLP Metrics exporter builder with optional HTTP/protobuf and gRPC transports.

use std::time::Duration;

use opentelemetry_c_abi::{OtelStatus, OtelStringView};

use crate::error::{clear_last_error, fail, fail_abi, fail_owned};
use crate::handle::{
    checked_mut, checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasMagic,
};
use crate::metric_exporter::{MetricExporterImpl, OtelMetricExporter};

#[cfg(feature = "otlp-grpc")]
use opentelemetry_otlp::{tonic_types::metadata::MetadataMap, WithTonicConfig};
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
use opentelemetry_otlp::{Compression as OtlpCompression, MetricExporter, WithExportConfig};
#[cfg(feature = "otlp-http")]
use opentelemetry_otlp::{Protocol, WithHttpConfig};
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
use opentelemetry_sdk::metrics::Temporality;
#[cfg(feature = "otlp-http")]
use std::collections::HashMap;
#[cfg(feature = "otlp-grpc")]
use tonic::metadata::{Ascii, MetadataKey, MetadataValue};

const BUILDER_MAGIC: u64 = 0x4F54_4C43_4D4F_544C;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Transport {
    #[default]
    HttpProtobuf,
    Grpc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Compression {
    #[default]
    None,
    Gzip,
    Zstd,
}

#[derive(Default)]
struct Config {
    endpoint: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Option<Duration>,
    temporality: u32,
    transport: Transport,
    compression: Compression,
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

/// Select the OTLP Metrics transport: 0=HTTP/protobuf, 1=gRPC.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_set_transport(
    builder: *mut OtelOtlpMetricExporterBuilder,
    transport: u32,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.transport = match transport {
                0 => Transport::HttpProtobuf,
                1 => Transport::Grpc,
                _ => {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "unknown OTLP metric transport value",
                    )
                }
            };
            OtelStatus::Ok
        })
    }
}

/// Select OTLP compression: 0=none/default, 1=gzip, 2=zstd.
///
/// # Safety
///
/// `builder` must be a live builder and must not be used concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_metric_exporter_builder_set_compression(
    builder: *mut OtelOtlpMetricExporterBuilder,
    compression: u32,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.compression = match compression {
                0 => Compression::None,
                1 => Compression::Gzip,
                2 => Compression::Zstd,
                _ => {
                    return fail(
                        OtelStatus::InvalidArgument,
                        "unknown OTLP metric compression value",
                    )
                }
            };
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

#[cfg(feature = "otlp-http")]
fn build_http_exporter(config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
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
    if let Some(compression) = configured_compression(config.compression) {
        builder = builder.with_compression(compression);
    }
    if let Some(temporality) = configured_temporality(config.temporality) {
        builder = builder.with_temporality(temporality);
    }
    builder
        .build()
        .map(MetricExporterImpl::OtlpHttp)
        .map_err(|err| {
            fail_owned(
                OtelStatus::InvalidConfig,
                format!("failed to build OTLP HTTP metric exporter: {err}"),
            )
        })
}

#[cfg(not(feature = "otlp-http"))]
fn build_http_exporter(_config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    Err(fail(
        OtelStatus::InvalidConfig,
        "OTLP HTTP/protobuf Metrics transport is unavailable: rebuild with `otlp-http`",
    ))
}

#[cfg(feature = "otlp-grpc")]
fn build_grpc_metadata(headers: &[(String, String)]) -> Result<MetadataMap, OtelStatus> {
    let mut metadata = MetadataMap::new();
    for (key, value) in headers {
        if key.to_ascii_lowercase().ends_with("-bin") {
            return Err(fail_owned(
                OtelStatus::InvalidArgument,
                format!("binary gRPC metadata is unsupported for key: {key}"),
            ));
        }
        let metadata_key = MetadataKey::<Ascii>::from_bytes(key.as_bytes()).map_err(|_| {
            fail_owned(
                OtelStatus::InvalidArgument,
                format!("invalid gRPC metadata key: {key}"),
            )
        })?;
        let metadata_value = MetadataValue::<Ascii>::try_from(value.as_str()).map_err(|_| {
            fail_owned(
                OtelStatus::InvalidArgument,
                format!("invalid gRPC metadata value for key: {key}"),
            )
        })?;
        metadata.insert(metadata_key, metadata_value);
    }
    Ok(metadata)
}

#[cfg(feature = "otlp-grpc")]
fn build_grpc_exporter(config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    let metadata = build_grpc_metadata(&config.headers)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(2)
        .thread_name("otel-c-otlp-grpc")
        .enable_all()
        .build()
        .map_err(|err| {
            fail_owned(
                OtelStatus::InternalError,
                format!("failed to create OTLP gRPC runtime: {err}"),
            )
        })?;

    let exporter = {
        let _runtime_guard = runtime.enter();
        let mut builder = MetricExporter::builder().with_tonic();
        if let Some(endpoint) = &config.endpoint {
            builder = builder.with_endpoint(endpoint.clone());
        }
        if let Some(timeout) = config.timeout {
            builder = builder.with_timeout(timeout);
        }
        if !metadata.is_empty() {
            builder = builder.with_metadata(metadata);
        }
        if let Some(compression) = configured_compression(config.compression) {
            builder = builder.with_compression(compression);
        }
        if let Some(temporality) = configured_temporality(config.temporality) {
            builder = builder.with_temporality(temporality);
        }
        builder.build().map_err(|err| {
            fail_owned(
                OtelStatus::InvalidConfig,
                format!("failed to build OTLP gRPC metric exporter: {err}"),
            )
        })?
    };

    Ok(MetricExporterImpl::OtlpGrpc(
        crate::metric_exporter::GrpcMetricExporter::new(exporter, runtime),
    ))
}

#[cfg(not(feature = "otlp-grpc"))]
fn build_grpc_exporter(_config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    Err(fail(
        OtelStatus::InvalidConfig,
        "OTLP gRPC Metrics transport is unavailable: rebuild with `otlp-grpc`",
    ))
}

fn build_exporter(config: &Config) -> Result<MetricExporterImpl, OtelStatus> {
    match config.transport {
        Transport::HttpProtobuf => build_http_exporter(config),
        Transport::Grpc => build_grpc_exporter(config),
    }
}

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
fn configured_temporality(preference: u32) -> Option<Temporality> {
    match preference {
        0 => None,
        1 => Some(Temporality::Cumulative),
        2 => Some(Temporality::Delta),
        3 => Some(Temporality::LowMemory),
        _ => unreachable!("validated by the public setter"),
    }
}

#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
fn configured_compression(compression: Compression) -> Option<OtlpCompression> {
    match compression {
        Compression::None => None,
        Compression::Gzip => Some(OtlpCompression::Gzip),
        Compression::Zstd => Some(OtlpCompression::Zstd),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use opentelemetry::metrics::MeterProvider;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    };

    fn sv(value: &str) -> OtelStringView {
        OtelStringView {
            ptr: value.as_ptr().cast(),
            len: value.len(),
        }
    }

    #[test]
    fn endpoint_and_timeout_setters_validate_and_use_last_value() {
        unsafe {
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(
                    std::ptr::null_mut(),
                    sv("http://localhost:4318/v1/metrics"),
                ),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_timeout_millis(std::ptr::null_mut(), 10),
                OtelStatus::InvalidArgument
            );

            let dead = Box::into_raw(Box::new(OtelOtlpMetricExporterBuilder {
                magic: 0,
                config: Config::default(),
            }));
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(dead, sv("http://localhost")),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_timeout_millis(dead, 10),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(dead, 1),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(dead, 1),
                OtelStatus::InvalidArgument
            );
            drop(Box::from_raw(dead));

            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(builder, OtelStringView::empty()),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.endpoint.as_deref(), Some(""));

            let first_endpoint = "http://127.0.0.1:4318/first";
            let final_endpoint = "http://127.0.0.1:4318/v1/metrics";
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(builder, sv(first_endpoint)),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(builder, sv(final_endpoint)),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.endpoint.as_deref(), Some(final_endpoint));

            assert_eq!(
                otel_otlp_metric_exporter_builder_set_timeout_millis(builder, 125),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.timeout, Some(Duration::from_millis(125)));
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_timeout_millis(builder, 250),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.timeout, Some(Duration::from_millis(250)));
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_timeout_millis(builder, 0),
                OtelStatus::Ok
            );
            assert!(
                (*builder).config.timeout.is_none(),
                "zero must preserve the exporter default timeout"
            );

            let invalid_utf8 = [0xff_u8];
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(
                    builder,
                    OtelStringView {
                        ptr: invalid_utf8.as_ptr().cast(),
                        len: invalid_utf8.len(),
                    },
                ),
                OtelStatus::InvalidUtf8
            );
            assert!(crate::api_ffi::test_probe::last_error()
                .to_ascii_lowercase()
                .contains("utf-8"));
            assert_eq!(
                (*builder).config.endpoint.as_deref(),
                Some(final_endpoint),
                "a rejected endpoint must not corrupt the live builder"
            );

            #[cfg(feature = "otlp-http")]
            {
                let mut exporter = std::ptr::null_mut();
                assert_eq!(
                    otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                    OtelStatus::Ok
                );
                assert!(!exporter.is_null());
                crate::metric_exporter::otel_metric_exporter_destroy(exporter);
            }

            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn transport_and_compression_setters_validate_and_use_last_value() {
        unsafe {
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(std::ptr::null_mut(), 1),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(std::ptr::null_mut(), 1),
                OtelStatus::InvalidArgument
            );

            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!((*builder).config.transport, Transport::HttpProtobuf);
            assert_eq!((*builder).config.compression, Compression::None);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.transport, Transport::Grpc);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 0),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.transport, Transport::HttpProtobuf);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 2),
                OtelStatus::InvalidArgument
            );
            assert_eq!((*builder).config.transport, Transport::HttpProtobuf);

            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.compression, Compression::Gzip);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(builder, 2),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.compression, Compression::Zstd);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(builder, 0),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.compression, Compression::None);
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(builder, 3),
                OtelStatus::InvalidArgument
            );
            assert_eq!((*builder).config.compression, Compression::None);
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn unavailable_requested_transport_fails_without_fallback() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            #[allow(unused_mut)]
            let mut exporter: *mut OtelMetricExporter = std::ptr::null_mut();

            #[cfg(not(feature = "otlp-http"))]
            {
                assert_eq!(
                    otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                    OtelStatus::InvalidConfig
                );
                assert!(exporter.is_null());
                assert!(crate::api_ffi::test_probe::last_error().contains("otlp-http"));
            }

            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            #[cfg(not(feature = "otlp-grpc"))]
            {
                assert_eq!(
                    otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                    OtelStatus::InvalidConfig
                );
                assert!(exporter.is_null());
                assert!(crate::api_ffi::test_probe::last_error().contains("otlp-grpc"));
            }

            assert!(exporter.is_null());
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn grpc_build_validates_metadata_and_owns_runtime() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(builder, sv("http://127.0.0.1:9"),),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_add_header(
                    builder,
                    sv("authorization-bin"),
                    sv("not-binary"),
                ),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::InvalidArgument
            );
            assert!(exporter.is_null());
            let error = crate::api_ffi::test_probe::last_error();
            assert!(error.contains("authorization-bin"));
            assert!(!error.contains("not-binary"));

            (*builder).config.headers.clear();
            assert_eq!(
                otel_otlp_metric_exporter_builder_add_header(
                    builder,
                    sv("x-tenant"),
                    sv("integration"),
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::Ok
            );
            assert!(!exporter.is_null());
            crate::metric_exporter::otel_metric_exporter_destroy(exporter);

            (*builder).config.headers.clear();
            assert_eq!(
                otel_otlp_metric_exporter_builder_add_header(
                    builder,
                    sv("x-secret"),
                    sv("private\nvalue"),
                ),
                OtelStatus::Ok
            );
            exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::InvalidArgument
            );
            assert!(exporter.is_null());
            let error = crate::api_ffi::test_probe::last_error();
            assert!(error.contains("x-secret"));
            assert!(!error.contains("private"));
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn grpc_rejects_invalid_endpoint() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(builder, sv("not a URI")),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::InvalidConfig
            );
            assert!(exporter.is_null());
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[cfg(all(feature = "otlp-grpc", not(feature = "grpc-tls-ring")))]
    #[test]
    fn grpc_https_requires_tls_feature() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_endpoint(
                    builder,
                    sv("https://localhost:4317"),
                ),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::InvalidConfig
            );
            assert!(exporter.is_null());
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[cfg(all(feature = "otlp-grpc", not(feature = "otlp-grpc-gzip")))]
    #[test]
    fn grpc_rejects_unavailable_compression() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_compression(builder, 1),
                OtelStatus::Ok
            );
            let mut exporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_metric_exporter_builder_build(builder, &mut exporter),
                OtelStatus::InvalidConfig
            );
            assert!(exporter.is_null());
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn setters_validate_temporality_and_duplicate_headers() {
        unsafe {
            let builder = otel_otlp_metric_exporter_builder_new();
            assert_eq!(
                otel_otlp_metric_exporter_builder_set_temporality(builder, 4),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_add_header(
                    builder,
                    OtelStringView {
                        ptr: b"x-test".as_ptr().cast(),
                        len: 6,
                    },
                    OtelStringView {
                        ptr: b"one".as_ptr().cast(),
                        len: 3,
                    },
                ),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_metric_exporter_builder_add_header(
                    builder,
                    OtelStringView {
                        ptr: b"X-Test".as_ptr().cast(),
                        len: 6,
                    },
                    OtelStringView {
                        ptr: b"two".as_ptr().cast(),
                        len: 3,
                    },
                ),
                OtelStatus::InvalidArgument
            );
            otel_otlp_metric_exporter_builder_destroy(builder);
        }
    }

    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    fn exported_temporalities(preference: u32) -> (Temporality, Temporality) {
        let selected = configured_temporality(preference).unwrap_or_default();
        let exporter = InMemoryMetricExporterBuilder::new()
            .with_temporality(selected)
            .build();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("temporality");
        meter.u64_counter("counter").build().add(3, &[]);
        meter.i64_up_down_counter("up_down").build().add(-2, &[]);
        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        let mut counter = None;
        let mut up_down = None;
        for metric in metrics
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
        {
            match (metric.name(), metric.data()) {
                ("counter", AggregatedMetrics::U64(MetricData::Sum(sum))) => {
                    counter = Some(sum.temporality())
                }
                ("up_down", AggregatedMetrics::I64(MetricData::Sum(sum))) => {
                    up_down = Some(sum.temporality())
                }
                _ => {}
            }
        }
        provider.shutdown().unwrap();
        (counter.unwrap(), up_down.unwrap())
    }

    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    #[test]
    fn configured_temporality_drives_exported_aggregation() {
        assert_eq!(
            exported_temporalities(1),
            (Temporality::Cumulative, Temporality::Cumulative)
        );
        assert_eq!(
            exported_temporalities(2),
            (Temporality::Delta, Temporality::Cumulative)
        );
        assert_eq!(
            exported_temporalities(3),
            (Temporality::Delta, Temporality::Cumulative)
        );
    }
}
