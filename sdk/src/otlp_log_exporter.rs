//! OTLP Logs exporter builder with optional HTTP/protobuf and gRPC transports.
//!
//! Mirrors [`crate::otlp_metric_exporter`]: transport and compression are selected through
//! signal-neutral numeric C selectors, and every combination that the crate was not compiled
//! with fails with `OTEL_STATUS_INVALID_CONFIG` and a message naming the required cargo
//! feature, so the symbol set stays identical across feature configurations.

use std::time::Duration;

use opentelemetry_c_abi::{
    OtelHandleHeader, OtelStatus, OtelStringView, OTEL_HANDLE_KIND_OTLP_LOG_EXPORTER_BUILDER,
};

use crate::error::{clear_last_error, fail, fail_abi, fail_owned};
use crate::handle::{
    checked_mut, checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw,
    HasHandleHeader,
};
use crate::log_exporter::{LogExporterImpl, OtelLogExporter};

#[cfg(feature = "otlp-grpc")]
use opentelemetry_otlp::{tonic_types::metadata::MetadataMap, WithTonicConfig};
#[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
use opentelemetry_otlp::{Compression as OtlpCompression, LogExporter, WithExportConfig};
#[cfg(feature = "otlp-http")]
use opentelemetry_otlp::{Protocol, WithHttpConfig};
#[cfg(feature = "otlp-http")]
use std::collections::HashMap;
#[cfg(feature = "otlp-grpc")]
use tonic::metadata::{Ascii, MetadataKey, MetadataValue};

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
    transport: Transport,
    compression: Compression,
}

/// Opaque OTLP Logs exporter builder. Not thread-safe; confine to one thread.
#[repr(C)]
pub struct OtelOtlpLogExporterBuilder {
    header: OtelHandleHeader,
    config: Config,
}

impl HasHandleHeader for OtelOtlpLogExporterBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_OTLP_LOG_EXPORTER_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Create an OTLP Logs exporter builder. Release with
/// `otel_otlp_log_exporter_builder_destroy()`.
#[no_mangle]
pub extern "C" fn otel_otlp_log_exporter_builder_new() -> *mut OtelOtlpLogExporterBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelOtlpLogExporterBuilder {
            header: OtelHandleHeader::new(OtelOtlpLogExporterBuilder::KIND),
            config: Config::default(),
        })
    })
}

/// Destroy an OTLP Logs exporter builder (no-op on NULL).
///
/// # Safety
/// `builder` must be NULL or a live builder not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_destroy(
    builder: *mut OtelOtlpLogExporterBuilder,
) {
    guard_unit(|| unsafe { destroy(builder) });
}

/// # Safety
/// `builder` must satisfy the handle contract (single-threaded).
unsafe fn with_config<F>(builder: *mut OtelOtlpLogExporterBuilder, f: F) -> OtelStatus
where
    F: FnOnce(&mut Config) -> OtelStatus,
{
    guard_status(|| {
        clear_last_error();
        match unsafe { checked_mut::<OtelOtlpLogExporterBuilder>(builder) } {
            Some(builder) => f(&mut builder.config),
            None => OtelStatus::InvalidArgument,
        }
    })
}

/// Set the full OTLP logs endpoint URL, used as-is (e.g. `http://localhost:4318/v1/logs`).
///
/// # Safety
/// `builder` and `endpoint` must satisfy their contracts.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_set_endpoint(
    builder: *mut OtelOtlpLogExporterBuilder,
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

/// Add a header (HTTP) / metadata entry (gRPC) sent with every OTLP export request.
///
/// Duplicate keys are rejected case-insensitively so a later value never silently replaces an
/// earlier one. The value is never included in any diagnostic message, because these headers
/// routinely carry credentials.
///
/// # Safety
/// `builder`, `key`, and `value` must satisfy their contracts.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_add_header(
    builder: *mut OtelOtlpLogExporterBuilder,
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
                        "OTLP header key must not be empty",
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
                    format!("OTLP header key already exists: {key}"),
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

/// Set the OTLP export request timeout in milliseconds (`0` == exporter default).
///
/// # Safety
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_set_timeout_millis(
    builder: *mut OtelOtlpLogExporterBuilder,
    timeout_millis: u64,
) -> OtelStatus {
    unsafe {
        with_config(builder, |config| {
            config.timeout = (timeout_millis != 0).then(|| Duration::from_millis(timeout_millis));
            OtelStatus::Ok
        })
    }
}

/// Select the OTLP Logs transport: 0=HTTP/protobuf, 1=gRPC.
///
/// # Safety
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_set_transport(
    builder: *mut OtelOtlpLogExporterBuilder,
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
                        "unknown OTLP log transport value",
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
/// `builder` must satisfy the handle contract.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_set_compression(
    builder: *mut OtelOtlpLogExporterBuilder,
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
                        "unknown OTLP log compression value",
                    )
                }
            };
            OtelStatus::Ok
        })
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

#[cfg(feature = "otlp-http")]
fn build_http_exporter(config: &Config) -> Result<LogExporterImpl, OtelStatus> {
    let mut builder = LogExporter::builder()
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
    builder
        .build()
        .map(LogExporterImpl::OtlpHttp)
        .map_err(|err| {
            fail_owned(
                OtelStatus::InvalidConfig,
                format!("failed to build OTLP HTTP log exporter: {err}"),
            )
        })
}

#[cfg(not(feature = "otlp-http"))]
fn build_http_exporter(_config: &Config) -> Result<LogExporterImpl, OtelStatus> {
    Err(fail(
        OtelStatus::InvalidConfig,
        "OTLP HTTP/protobuf Logs transport is unavailable: rebuild with `otlp-http`",
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
        // The value is deliberately excluded from the message: these entries carry secrets.
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
fn build_grpc_exporter(config: &Config) -> Result<LogExporterImpl, OtelStatus> {
    let metadata = build_grpc_metadata(&config.headers)?;
    let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
    runtime_builder
        .worker_threads(1)
        .max_blocking_threads(2)
        .thread_name("otel-c-otlp-logs-grpc")
        .enable_all();
    let runtime =
        crate::metric_exporter::GrpcRuntimeGuard::new(runtime_builder.build().map_err(|err| {
            fail_owned(
                OtelStatus::InternalError,
                format!("failed to create OTLP gRPC runtime: {err}"),
            )
        })?);

    let exporter = {
        let _runtime_guard = runtime.runtime().enter();
        let mut builder = LogExporter::builder().with_tonic();
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
        builder.build()
    };
    let exporter = exporter.map_err(|err| {
        fail_owned(
            OtelStatus::InvalidConfig,
            format!("failed to build OTLP gRPC log exporter: {err}"),
        )
    })?;
    Ok(LogExporterImpl::OtlpGrpc(
        crate::log_exporter::GrpcLogExporter::new(exporter, runtime),
    ))
}

#[cfg(not(feature = "otlp-grpc"))]
fn build_grpc_exporter(_config: &Config) -> Result<LogExporterImpl, OtelStatus> {
    Err(fail(
        OtelStatus::InvalidConfig,
        "OTLP gRPC Logs transport is unavailable: rebuild with `otlp-grpc`",
    ))
}

fn validate_transport_available(transport: Transport) -> Result<(), OtelStatus> {
    match transport {
        Transport::HttpProtobuf if !cfg!(feature = "otlp-http") => Err(fail(
            OtelStatus::InvalidConfig,
            "OTLP HTTP/protobuf Logs transport is unavailable: rebuild with `otlp-http`",
        )),
        Transport::Grpc if !cfg!(feature = "otlp-grpc") => Err(fail(
            OtelStatus::InvalidConfig,
            "OTLP gRPC Logs transport is unavailable: rebuild with `otlp-grpc`",
        )),
        _ => Ok(()),
    }
}

fn validate_compression_available(
    transport: Transport,
    compression: Compression,
) -> Result<(), OtelStatus> {
    let missing = match (transport, compression) {
        (Transport::HttpProtobuf, Compression::Gzip) if !cfg!(feature = "otlp-http-gzip") => {
            Some((
                "OTLP HTTP/protobuf gzip compression is unavailable",
                "otlp-http-gzip",
            ))
        }
        (Transport::HttpProtobuf, Compression::Zstd) if !cfg!(feature = "otlp-http-zstd") => {
            Some((
                "OTLP HTTP/protobuf zstd compression is unavailable",
                "otlp-http-zstd",
            ))
        }
        (Transport::Grpc, Compression::Gzip) if !cfg!(feature = "otlp-grpc-gzip") => Some((
            "OTLP gRPC gzip compression is unavailable",
            "otlp-grpc-gzip",
        )),
        (Transport::Grpc, Compression::Zstd) if !cfg!(feature = "otlp-grpc-zstd") => Some((
            "OTLP gRPC zstd compression is unavailable",
            "otlp-grpc-zstd",
        )),
        _ => None,
    };
    match missing {
        Some((message, feature)) => Err(fail_owned(
            OtelStatus::InvalidConfig,
            format!("{message}: rebuild with `{feature}`"),
        )),
        None => Ok(()),
    }
}

fn build_exporter(config: &Config) -> Result<LogExporterImpl, OtelStatus> {
    validate_transport_available(config.transport)?;
    validate_compression_available(config.transport, config.compression)?;
    match config.transport {
        Transport::HttpProtobuf => build_http_exporter(config),
        Transport::Grpc => build_grpc_exporter(config),
    }
}

/// Build an owned OTLP Logs exporter.
///
/// # Safety
///
/// `builder` must be live and `out` must address writable storage.
#[no_mangle]
pub unsafe extern "C" fn otel_otlp_log_exporter_builder_build(
    builder: *const OtelOtlpLogExporterBuilder,
    out: *mut *mut OtelLogExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        let builder = match unsafe { checked_ref::<OtelOtlpLogExporterBuilder>(builder) } {
            Some(builder) => builder,
            None => return OtelStatus::InvalidArgument,
        };
        #[cfg(any(feature = "otlp-http", feature = "otlp-grpc", test))]
        {
            let exporter = match build_exporter(&builder.config) {
                Ok(exporter) => exporter,
                Err(status) => return status,
            };
            unsafe { *out = into_raw(OtelLogExporter::new(exporter)) };
            OtelStatus::Ok
        }
        #[cfg(not(any(feature = "otlp-http", feature = "otlp-grpc", test)))]
        {
            match build_exporter(&builder.config) {
                Err(status) => status,
                Ok(exporter) => match exporter {},
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "otlp-http", feature = "otlp-grpc"))]
    use crate::log_exporter::otel_log_exporter_destroy;

    fn sv(value: &str) -> OtelStringView {
        OtelStringView {
            ptr: value.as_ptr().cast(),
            len: value.len(),
        }
    }

    fn poisoned_builder() -> *mut OtelOtlpLogExporterBuilder {
        Box::into_raw(Box::new(OtelOtlpLogExporterBuilder {
            header: {
                let mut header = OtelHandleHeader::new(OtelOtlpLogExporterBuilder::KIND);
                header.poison();
                header
            },
            config: Config::default(),
        }))
    }

    #[test]
    fn every_setter_rejects_null_and_destroyed_builders() {
        unsafe {
            let dead = poisoned_builder();
            for builder in [std::ptr::null_mut(), dead] {
                assert_eq!(
                    otel_otlp_log_exporter_builder_set_endpoint(builder, sv("http://localhost")),
                    OtelStatus::InvalidArgument
                );
                assert_eq!(
                    otel_otlp_log_exporter_builder_add_header(builder, sv("k"), sv("v")),
                    OtelStatus::InvalidArgument
                );
                assert_eq!(
                    otel_otlp_log_exporter_builder_set_timeout_millis(builder, 10),
                    OtelStatus::InvalidArgument
                );
                assert_eq!(
                    otel_otlp_log_exporter_builder_set_transport(builder, 0),
                    OtelStatus::InvalidArgument
                );
                assert_eq!(
                    otel_otlp_log_exporter_builder_set_compression(builder, 0),
                    OtelStatus::InvalidArgument
                );
                let mut out: *mut OtelLogExporter = std::ptr::null_mut();
                assert_eq!(
                    otel_otlp_log_exporter_builder_build(builder, &mut out),
                    OtelStatus::InvalidArgument
                );
                assert!(out.is_null());
            }
            drop(Box::from_raw(dead));
            otel_otlp_log_exporter_builder_destroy(std::ptr::null_mut());
        }
    }

    #[test]
    fn unknown_transport_and_compression_selectors_are_rejected() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_set_transport(builder, 2),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_set_transport(builder, u32::MAX),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_set_compression(builder, 3),
                OtelStatus::InvalidArgument
            );
            // A rejected selector must not corrupt the previously accepted configuration.
            assert_eq!((*builder).config.transport, Transport::HttpProtobuf);
            assert_eq!((*builder).config.compression, Compression::None);
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn header_keys_must_be_non_empty_and_unique_case_insensitively() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_add_header(builder, sv(""), sv("v")),
                OtelStatus::InvalidArgument
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_add_header(builder, sv("Api-Key"), sv("secret")),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_add_header(builder, sv("api-key"), sv("other")),
                OtelStatus::InvalidArgument
            );
            // The rejection must never echo the credential back to the caller.
            let message = crate::api_ffi::test_probe::last_error();
            assert!(message.contains("api-key"), "{message}");
            assert!(!message.contains("other"), "{message}");
            assert_eq!((*builder).config.headers.len(), 1);
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn endpoint_and_timeout_use_the_last_value_and_zero_means_default() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_set_endpoint(builder, sv("http://a/v1/logs")),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_set_endpoint(builder, sv("http://b/v1/logs")),
                OtelStatus::Ok
            );
            assert_eq!(
                (*builder).config.endpoint.as_deref(),
                Some("http://b/v1/logs")
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_set_timeout_millis(builder, 250),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.timeout, Some(Duration::from_millis(250)));
            assert_eq!(
                otel_otlp_log_exporter_builder_set_timeout_millis(builder, 0),
                OtelStatus::Ok
            );
            assert_eq!((*builder).config.timeout, None);
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }

    #[test]
    fn transport_availability_matches_the_compiled_feature_set() {
        assert_eq!(
            validate_transport_available(Transport::HttpProtobuf).is_ok(),
            cfg!(feature = "otlp-http")
        );
        assert_eq!(
            validate_transport_available(Transport::Grpc).is_ok(),
            cfg!(feature = "otlp-grpc")
        );
    }

    #[test]
    fn compression_availability_matches_the_compiled_feature_set() {
        // `None` is always available; every explicit algorithm is gated per transport.
        for transport in [Transport::HttpProtobuf, Transport::Grpc] {
            assert!(validate_compression_available(transport, Compression::None).is_ok());
        }
        assert_eq!(
            validate_compression_available(Transport::HttpProtobuf, Compression::Gzip).is_ok(),
            cfg!(feature = "otlp-http-gzip")
        );
        assert_eq!(
            validate_compression_available(Transport::HttpProtobuf, Compression::Zstd).is_ok(),
            cfg!(feature = "otlp-http-zstd")
        );
        assert_eq!(
            validate_compression_available(Transport::Grpc, Compression::Gzip).is_ok(),
            cfg!(feature = "otlp-grpc-gzip")
        );
        assert_eq!(
            validate_compression_available(Transport::Grpc, Compression::Zstd).is_ok(),
            cfg!(feature = "otlp-grpc-zstd")
        );
    }

    #[test]
    fn build_rejects_a_null_out_pointer() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_build(builder, std::ptr::null_mut()),
                OtelStatus::InvalidArgument
            );
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }

    #[cfg(feature = "otlp-http")]
    #[test]
    fn http_build_succeeds_without_contacting_the_endpoint() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_set_endpoint(
                    builder,
                    sv("http://127.0.0.1:4318/v1/logs"),
                ),
                OtelStatus::Ok
            );
            let mut exporter: *mut OtelLogExporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_log_exporter_builder_build(builder, &mut exporter),
                OtelStatus::Ok
            );
            assert!(!exporter.is_null());
            // The builder is reusable: building twice yields two independent exporters.
            let mut second: *mut OtelLogExporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_log_exporter_builder_build(builder, &mut second),
                OtelStatus::Ok
            );
            assert!(!second.is_null() && second != exporter);
            otel_log_exporter_destroy(exporter);
            otel_log_exporter_destroy(second);
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }

    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn grpc_metadata_rejects_binary_keys_and_invalid_values() {
        assert!(build_grpc_metadata(&[("x-trace-bin".into(), "v".into())]).is_err());
        assert!(build_grpc_metadata(&[("bad key".into(), "v".into())]).is_err());
        assert!(build_grpc_metadata(&[("ok".into(), "\n".into())]).is_err());
        assert!(build_grpc_metadata(&[("ok".into(), "value".into())]).is_ok());
        // Mixed case must be rejected for `-bin` too, which is why the suffix check
        // lowercases before comparing rather than matching the raw bytes.
        assert!(build_grpc_metadata(&[("X-Trace-BIN".into(), "v".into())]).is_err());
    }

    /// gRPC metadata keys are case-insensitive on the wire and canonically lowercase.
    ///
    /// A C caller writing `X-Tenant` must not have that header silently dropped, nor have it
    /// fail late during exporter construction. Tonic normalizes on `from_bytes`, so no
    /// lowercasing is needed here -- but that is an upstream behaviour this bridge depends
    /// on, so it is pinned by a test rather than assumed.
    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn grpc_metadata_keys_are_normalized_to_lowercase() {
        let metadata = build_grpc_metadata(&[
            ("X-Tenant".into(), "acme".into()),
            ("Authorization".into(), "Bearer token".into()),
        ])
        .expect("mixed-case keys must be accepted, not deferred to build time");

        assert_eq!(
            metadata.get("x-tenant").map(|v| v.to_str().unwrap()),
            Some("acme"),
            "X-Tenant must be retrievable as x-tenant"
        );
        assert_eq!(
            metadata.get("authorization").map(|v| v.to_str().unwrap()),
            Some("Bearer token"),
            "only the key is normalized; the value must survive byte-for-byte"
        );
        for key in metadata.keys() {
            let tonic::metadata::KeyRef::Ascii(name) = key else {
                panic!("binary metadata keys are rejected before reaching the map");
            };
            let name = name.as_str();
            assert_eq!(name, name.to_ascii_lowercase(), "{name} is not lowercase");
        }
    }

    #[cfg(feature = "otlp-grpc")]
    #[test]
    fn grpc_build_creates_its_own_runtime_and_shuts_it_down() {
        unsafe {
            let builder = otel_otlp_log_exporter_builder_new();
            assert_eq!(
                otel_otlp_log_exporter_builder_set_transport(builder, 1),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_otlp_log_exporter_builder_set_endpoint(builder, sv("http://127.0.0.1:4317")),
                OtelStatus::Ok
            );
            let mut exporter: *mut OtelLogExporter = std::ptr::null_mut();
            assert_eq!(
                otel_otlp_log_exporter_builder_build(builder, &mut exporter),
                OtelStatus::Ok
            );
            assert!(!exporter.is_null());
            otel_log_exporter_destroy(exporter);
            otel_otlp_log_exporter_builder_destroy(builder);
        }
    }
}
