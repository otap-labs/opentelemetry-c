//! # opentelemetry-c-sdk
//!
//! The **C SDK** of the `opentelemetry-c` split. It follows idiomatic OpenTelemetry layering:
//! the SDK core (builder + `SdkTracerProvider`) is a separate concern from **span processors**
//! and **exporters**, which are generic, opaque extension points:
//!
//! - `otel_trace_exporter_t` wraps an internal `TraceExporterImpl` (a `SpanExporter`). The OTLP
//!   HTTP/protobuf exporter is one **optional** variant (cargo feature `otlp-http`, enabled by
//!   default through the `otlp` compatibility alias), built by `otlp_exporter`.
//! - `otel_span_processor_t` wraps an internal `SpanProcessorImpl` (a `SpanProcessor`). The
//!   batch span processor is one variant (SDK core), built by `batch_processor`.
//! - The SDK builder (`sdk`) stores a homogeneous `Vec<SpanProcessorImpl>`, so it is coupled to
//!   neither OTLP nor the batch processor.
//!
//! With `--no-default-features` the SDK core builds without `opentelemetry-otlp`, `reqwest`, or
//! any TLS backend; the OTLP builder symbols remain but return `OTEL_STATUS_INVALID_CONFIG`.
//! Installing as global (or fetching a provider handle) registers this SDK's implementation
//! into the **API cdylib's** global provider slot across the C ABI, so API-only instrumentation
//! observes it.
//!
//! ## Linking model
//!
//! Applications link `libopentelemetry_c_sdk` **and** `libopentelemetry_c_api`. This cdylib
//! references the API's internal registration symbols (`otel_api_register_global_provider`,
//! `otel_api_provider_new`, `otel_api_set_last_error`, `otel_api_clear_last_error`), which
//! resolve against `libopentelemetry_c_api` at load time (see `build.rs`). This crate never
//! re-exports the public API/trace/common functions, so there are no duplicate symbols.

#![allow(unsafe_attr_outside_unsafe)]

// `reqwest` is an optional direct dependency (feature `otlp-http`) solely to select the OTLP
// blocking client's TLS backend via the `native-tls` / `rustls-tls` cargo features; it is
// never called directly.
#[cfg(feature = "otlp-http")]
use reqwest as _;

mod api_ffi;
mod batch_processor;
mod custom_log_exporter;
mod custom_metric_exporter;
mod error;
mod handle;
mod log_export_view;
mod log_exporter;
mod log_processor;
mod logs_vtable;
mod manual_metric_reader;
mod metric_batch;
mod metric_exporter;
mod metric_view;
mod metrics_vtable;
mod otlp_exporter;
mod otlp_log_exporter;
mod otlp_metric_exporter;
mod periodic_metric_reader;
mod sdk;
mod span_processor;
mod trace_exporter;
mod vtable;

pub use batch_processor::{
    otel_batch_span_processor_builder_build, otel_batch_span_processor_builder_destroy,
    otel_batch_span_processor_builder_new,
    otel_batch_span_processor_builder_set_export_timeout_millis,
    otel_batch_span_processor_builder_set_exporter,
    otel_batch_span_processor_builder_set_max_export_batch_size,
    otel_batch_span_processor_builder_set_max_queue_size,
    otel_batch_span_processor_builder_set_scheduled_delay_millis, OtelBatchSpanProcessorBuilder,
};
pub use custom_log_exporter::{
    otel_custom_log_exporter_new, OtelCustomLogExport, OtelCustomLogExporterCallbacks,
    OtelCustomLogShutdown, OtelCustomLogStateDestroy,
};
pub use custom_metric_exporter::{
    otel_custom_metric_exporter_new, OtelCustomMetricExporterCallbacks,
};
pub use log_export_view::{
    OtelLogExportBatchView, OtelLogExportRecordView, OtelLogExportScopeView,
    OTEL_LOG_EXPORT_FIELD_BODY, OTEL_LOG_EXPORT_FIELD_EVENT_NAME, OTEL_LOG_EXPORT_FIELD_KNOWN_MASK,
    OTEL_LOG_EXPORT_FIELD_OBSERVED_TIMESTAMP, OTEL_LOG_EXPORT_FIELD_SEVERITY_TEXT,
    OTEL_LOG_EXPORT_FIELD_TARGET, OTEL_LOG_EXPORT_FIELD_TIMESTAMP,
    OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT, OTEL_LOG_EXPORT_MAX_RECORDS,
};
pub use log_exporter::{otel_log_exporter_destroy, OtelLogExporter};
pub use log_processor::{
    otel_batch_log_processor_builder_build, otel_batch_log_processor_builder_destroy,
    otel_batch_log_processor_builder_new, otel_batch_log_processor_builder_set_exporter,
    otel_batch_log_processor_builder_set_max_export_batch_size,
    otel_batch_log_processor_builder_set_max_queue_size,
    otel_batch_log_processor_builder_set_scheduled_delay_millis, otel_log_processor_destroy,
    otel_simple_log_processor_create, OtelBatchLogProcessorBuilder, OtelLogProcessor,
};
pub use manual_metric_reader::{
    otel_manual_metric_reader_destroy, otel_manual_metric_reader_new, OtelManualMetricReader,
};
pub use metric_batch::{
    otel_metric_batch_visit, OtelMetricArrayView, OtelMetricAttribute, OtelMetricAttributeValue,
    OtelMetricBatch, OtelMetricExemplar, OtelMetricMetadata, OtelMetricNumber, OtelMetricPoint,
    OtelMetricVisitor,
};
pub use metric_exporter::{otel_metric_exporter_destroy, OtelMetricExporter};
pub use metric_view::{
    otel_metric_view_builder_add_allowed_attribute, otel_metric_view_builder_build,
    otel_metric_view_builder_destroy, otel_metric_view_builder_new,
    otel_metric_view_builder_set_aggregation, otel_metric_view_builder_set_cardinality_limit,
    otel_metric_view_builder_set_explicit_histogram,
    otel_metric_view_builder_set_exponential_histogram,
    otel_metric_view_builder_set_instrument_kind, otel_metric_view_builder_set_meter_name,
    otel_metric_view_builder_set_name_pattern, otel_metric_view_builder_set_output_description,
    otel_metric_view_builder_set_output_name, otel_metric_view_builder_set_output_unit,
    otel_metric_view_builder_set_unit, otel_metric_view_destroy, OtelMetricView,
    OtelMetricViewBuilder,
};
pub use otlp_exporter::{
    otel_otlp_trace_exporter_builder_add_header, otel_otlp_trace_exporter_builder_build,
    otel_otlp_trace_exporter_builder_destroy, otel_otlp_trace_exporter_builder_new,
    otel_otlp_trace_exporter_builder_set_endpoint,
    otel_otlp_trace_exporter_builder_set_timeout_millis, OtelOtlpTraceExporterBuilder,
};
pub use otlp_log_exporter::{
    otel_otlp_log_exporter_builder_add_header, otel_otlp_log_exporter_builder_build,
    otel_otlp_log_exporter_builder_destroy, otel_otlp_log_exporter_builder_new,
    otel_otlp_log_exporter_builder_set_compression, otel_otlp_log_exporter_builder_set_endpoint,
    otel_otlp_log_exporter_builder_set_timeout_millis,
    otel_otlp_log_exporter_builder_set_transport, OtelOtlpLogExporterBuilder,
};
pub use otlp_metric_exporter::{
    otel_otlp_metric_exporter_builder_add_header, otel_otlp_metric_exporter_builder_build,
    otel_otlp_metric_exporter_builder_destroy, otel_otlp_metric_exporter_builder_new,
    otel_otlp_metric_exporter_builder_set_compression,
    otel_otlp_metric_exporter_builder_set_endpoint,
    otel_otlp_metric_exporter_builder_set_temporality,
    otel_otlp_metric_exporter_builder_set_timeout_millis,
    otel_otlp_metric_exporter_builder_set_transport, OtelOtlpMetricExporterBuilder,
};
pub use periodic_metric_reader::{
    otel_periodic_metric_reader_builder_build, otel_periodic_metric_reader_builder_destroy,
    otel_periodic_metric_reader_builder_new, otel_periodic_metric_reader_builder_set_exporter,
    otel_periodic_metric_reader_builder_set_interval_millis,
    otel_periodic_metric_reader_builder_set_runtime,
    otel_periodic_metric_reader_builder_set_timeout_millis, otel_periodic_metric_reader_destroy,
    OtelPeriodicMetricReader, OtelPeriodicMetricReaderBuilder,
};
pub use sdk::{
    otel_sdk_build, otel_sdk_builder_add_log_processor, otel_sdk_builder_add_manual_metric_reader,
    otel_sdk_builder_add_metric_reader, otel_sdk_builder_add_metric_view,
    otel_sdk_builder_add_resource_attribute, otel_sdk_builder_add_span_processor,
    otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_builder_set_service_name,
    otel_sdk_destroy, otel_sdk_force_flush, otel_sdk_get_logger_provider,
    otel_sdk_get_meter_provider, otel_sdk_get_tracer_provider, otel_sdk_logs_force_flush,
    otel_sdk_logs_shutdown, otel_sdk_metrics_force_flush, otel_sdk_metrics_shutdown,
    otel_sdk_set_as_global, otel_sdk_set_logs_as_global, otel_sdk_set_metrics_as_global,
    otel_sdk_shutdown, OtelSdk, OtelSdkBuilder,
};
pub use span_processor::{otel_span_processor_destroy, OtelSpanProcessor};
pub use trace_exporter::{otel_trace_exporter_destroy, OtelTraceExporter};
