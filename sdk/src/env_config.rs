//! Environment-based SDK configuration shared by the C-facing builders.
//!
//! The pinned OpenTelemetry Rust builders own parsing for resources, samplers, batch
//! processors, periodic readers, and most OTLP exporter settings. This module handles only
//! orchestration that the C wrapper must decide before selecting an upstream builder.

use std::env;
use std::io::Write;

pub(crate) const OTEL_SDK_DISABLED: &str = "OTEL_SDK_DISABLED";
pub(crate) const OTEL_EXPORTER_OTLP_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_PROTOCOL";

pub(crate) const OTEL_EXPORTER_OTLP_TRACES_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL";
pub(crate) const OTEL_EXPORTER_OTLP_METRICS_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL";
pub(crate) const OTEL_EXPORTER_OTLP_LOGS_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OtlpProtocol {
    HttpProtobuf,
    Grpc,
}

pub(crate) fn warn(message: &str) {
    // Diagnostics are best-effort until the SDK exposes a diagnostic callback. Never turn a
    // harmless configuration warning into a failed C API call when stderr is closed/broken.
    let _ = writeln!(std::io::stderr(), "OpenTelemetry C SDK warning: {message}");
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn parse_protocol(value: &str) -> Option<OtlpProtocol> {
    if value.eq_ignore_ascii_case("http/protobuf") {
        Some(OtlpProtocol::HttpProtobuf)
    } else if value.eq_ignore_ascii_case("grpc") {
        Some(OtlpProtocol::Grpc)
    } else {
        None
    }
}

fn protocol_from<F>(signal_name: &str, mut get: F) -> OtlpProtocol
where
    F: FnMut(&str) -> Option<String>,
{
    for name in [signal_name, OTEL_EXPORTER_OTLP_PROTOCOL] {
        let Some(value) = get(name).filter(|value| !value.is_empty()) else {
            continue;
        };
        if let Some(protocol) = parse_protocol(&value) {
            return protocol;
        }
        warn(&format!(
            "ignoring unrecognized {name} value; supported values are grpc and http/protobuf"
        ));
    }
    OtlpProtocol::HttpProtobuf
}

pub(crate) fn otlp_protocol(signal_name: &str) -> OtlpProtocol {
    protocol_from(signal_name, nonempty_env)
}

fn sdk_disabled_from<F>(mut get: F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    let Some(value) = get(OTEL_SDK_DISABLED).filter(|value| !value.is_empty()) else {
        return false;
    };
    if value.eq_ignore_ascii_case("true") {
        true
    } else {
        if !value.eq_ignore_ascii_case("false") {
            warn("ignoring invalid OTEL_SDK_DISABLED value; only true and false are valid");
        }
        false
    }
}

pub(crate) fn sdk_disabled() -> bool {
    sdk_disabled_from(nonempty_env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn values(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn signal_protocol_precedes_generic_and_is_case_insensitive() {
        let env = values(&[
            (OTEL_EXPORTER_OTLP_PROTOCOL, "http/protobuf"),
            (OTEL_EXPORTER_OTLP_TRACES_PROTOCOL, "GRPC"),
        ]);
        assert_eq!(
            protocol_from(OTEL_EXPORTER_OTLP_TRACES_PROTOCOL, |name| env
                .get(name)
                .cloned()),
            OtlpProtocol::Grpc
        );
    }

    #[test]
    fn invalid_or_empty_specific_protocol_falls_back_to_generic() {
        for specific in ["", "http/json", "unknown"] {
            let env = values(&[
                (OTEL_EXPORTER_OTLP_PROTOCOL, "grpc"),
                (OTEL_EXPORTER_OTLP_LOGS_PROTOCOL, specific),
            ]);
            assert_eq!(
                protocol_from(OTEL_EXPORTER_OTLP_LOGS_PROTOCOL, |name| env
                    .get(name)
                    .cloned()),
                OtlpProtocol::Grpc
            );
        }
    }

    #[test]
    fn protocol_defaults_to_http_protobuf() {
        assert_eq!(
            protocol_from(OTEL_EXPORTER_OTLP_METRICS_PROTOCOL, |_| None),
            OtlpProtocol::HttpProtobuf
        );
    }

    #[test]
    fn sdk_disabled_accepts_only_true_as_enabled_value() {
        for value in ["true", "TRUE", "True"] {
            assert!(sdk_disabled_from(|_| Some(value.to_owned())));
        }
        for value in ["false", "FALSE", "", "1", "yes"] {
            assert!(!sdk_disabled_from(|_| Some(value.to_owned())));
        }
        assert!(!sdk_disabled_from(|_| None));
    }
}
