//! Cross-artifact proof: the API and SDK are **separate** dynamic libraries that share the
//! API-owned global provider slot.
//!
//! This test compiles a small C program, links it against BOTH `libopentelemetry_c_api` and
//! `libopentelemetry_c_sdk`, and runs it. The program installs the SDK and then emits spans
//! using ONLY the API's global-provider path (as an instrumentation library would). A
//! self-contained mock OTLP/HTTP collector (a plain `TcpListener`) confirms the spans were
//! exported through the SDK — proving the SDK registered into the API-owned global slot and
//! that API-only calls dispatch to it across the artifact boundary.
//!
//! The test **self-skips** when a C compiler is unavailable or the cdylibs have not been
//! built yet (run `cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk` first).
//! Self-skipping is a **local developer convenience only**: when `CI` is set the test
//! instead **fails hard** if either prerequisite is missing, so the cross-artifact proof
//! can never silently no-op in CI.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(feature = "otlp-grpc")]
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
#[cfg(feature = "otlp-grpc")]
use opentelemetry_proto::tonic::collector::metrics::v1::{
    metrics_service_server::{MetricsService, MetricsServiceServer},
    ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{any_value, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{metric, number_data_point};
use prost::Message;
#[cfg(feature = "otlp-grpc")]
use tokio::net::TcpListener as TokioTcpListener;
#[cfg(feature = "otlp-grpc")]
use tokio::sync::oneshot;
#[cfg(feature = "otlp-grpc")]
use tokio_stream::wrappers::TcpListenerStream;
#[cfg(feature = "otlp-grpc-gzip")]
use tonic::codec::CompressionEncoding;
#[cfg(feature = "otlp-grpc")]
use tonic::{Request, Response, Status};

fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    for candidate in ["cc", "clang", "gcc"] {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn dylib_names(stem: &str) -> [String; 3] {
    [
        format!("lib{stem}.dylib"),
        format!("lib{stem}.so"),
        format!("{stem}.dll"),
    ]
}

/// Whether we are running under CI. When true, this test must **fail** rather than
/// self-skip if its prerequisites (C compiler, built cdylibs) are missing — otherwise the
/// cross-artifact global-provider proof could silently never run in CI.
fn is_ci() -> bool {
    std::env::var("CI")
        .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

#[cfg(feature = "otlp-grpc")]
fn output_with_timeout(command: &mut Command, timeout: Duration) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn child process");
    let mut stdout = child.stdout.take().expect("capture child stdout");
    let mut stderr = child.stderr.take().expect("capture child stderr");
    let stdout_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).expect("read child stdout");
        output
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).expect("read child stderr");
        output
    });
    let deadline = std::time::Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("query child status") {
            break (status, false);
        }
        if std::time::Instant::now() >= deadline {
            child.kill().expect("terminate timed-out child");
            break (child.wait().expect("reap timed-out child"), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: stdout_thread.join().expect("join stdout reader"),
        stderr: stderr_thread.join().expect("join stderr reader"),
    };
    if timed_out {
        panic!(
            "child process timed out after {timeout:?}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

/// Find a target profile dir that contains BOTH cdylibs.
fn find_lib_dir() -> Option<PathBuf> {
    // This crate lives at `<workspace>/sdk`, so the workspace root is one parent up:
    // sdk -> <workspace>.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    // Honor CARGO_TARGET_DIR: an absolute value is used as-is; a relative value is resolved
    // against the workspace root (NOT the SDK crate dir). Otherwise default to <root>/target.
    let target_dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                workspace_root.join(dir)
            }
        }
        _ => workspace_root.join("target"),
    };
    // `scripts/test.sh` builds debug cdylibs immediately before this test. Prefer them over
    // potentially stale release artifacts left by an earlier benchmark run.
    for profile in ["debug", "release"] {
        let dir = target_dir.join(profile);
        let has = |stem: &str| dylib_names(stem).iter().any(|n| dir.join(n).exists());
        if has("opentelemetry_c_api") && has("opentelemetry_c_sdk") {
            return Some(dir);
        }
    }
    None
}

const HARNESS_C: &str = r#"
#include <stdint.h>
#include <string.h>
#include <stddef.h>
#include <stdlib.h>
typedef struct { const char* ptr; size_t len; } otel_string_view_t;
typedef union {
    otel_string_view_t string_value;
    uint8_t bool_value;
    int64_t int64_value;
    double double_value;
} otel_attribute_value_t;
typedef struct {
    otel_string_view_t key;
    uint32_t value_type;
    otel_attribute_value_t value;
} otel_key_value_t;
typedef struct {
    uint64_t struct_size;
    otel_string_view_t description;
    otel_string_view_t unit;
    const double* boundaries;
    size_t boundary_count;
} otel_instrument_options_t;
typedef struct otel_sdk_builder_t otel_sdk_builder_t;
typedef struct otel_sdk_t otel_sdk_t;
typedef struct otel_tracer_provider_t otel_tracer_provider_t;
typedef struct otel_tracer_t otel_tracer_t;
typedef struct otel_span_t otel_span_t;
typedef struct otel_meter_provider_t otel_meter_provider_t;
typedef struct otel_meter_t otel_meter_t;
typedef struct otel_counter_u64_t otel_counter_u64_t;
typedef struct otel_gauge_f64_t otel_gauge_f64_t;
typedef struct otel_histogram_f64_t otel_histogram_f64_t;
typedef struct otel_observable_gauge_u64_t otel_observable_gauge_u64_t;
typedef struct otel_observer_u64_t otel_observer_u64_t;
typedef struct { uint32_t kind; const otel_span_t* parent; } otel_span_start_options_t;
extern otel_tracer_provider_t* otel_global_tracer_provider(void);
extern otel_tracer_t* otel_tracer_provider_get_tracer(const otel_tracer_provider_t*, otel_string_view_t, otel_string_view_t, otel_string_view_t);
extern otel_span_t* otel_tracer_start_span(const otel_tracer_t*, otel_string_view_t, const otel_span_start_options_t*);
extern int otel_span_set_string_attribute(otel_span_t*, otel_string_view_t, otel_string_view_t);
extern int otel_span_end(otel_span_t*);
extern void otel_span_destroy(otel_span_t*);
extern void otel_tracer_destroy(otel_tracer_t*);
extern void otel_tracer_provider_destroy(otel_tracer_provider_t*);
extern otel_meter_provider_t* otel_global_meter_provider(void);
extern otel_meter_t* otel_meter_provider_get_meter(const otel_meter_provider_t*, otel_string_view_t, otel_string_view_t, otel_string_view_t);
extern int otel_meter_create_u64_counter(const otel_meter_t*, otel_string_view_t, const void*, otel_counter_u64_t**);
extern int otel_meter_create_f64_gauge(const otel_meter_t*, otel_string_view_t, const void*, otel_gauge_f64_t**);
extern int otel_meter_create_f64_histogram(const otel_meter_t*, otel_string_view_t, const void*, otel_histogram_f64_t**);
extern int otel_meter_create_u64_observable_gauge(const otel_meter_t*, otel_string_view_t, const void*, void (*)(otel_observer_u64_t*, void*), void*, void (*)(void*), otel_observable_gauge_u64_t**);
extern int otel_counter_u64_add(const otel_counter_u64_t*, uint64_t, const void*, size_t);
extern int otel_gauge_f64_record(const otel_gauge_f64_t*, double, const void*, size_t);
extern int otel_histogram_f64_record(const otel_histogram_f64_t*, double, const void*, size_t);
extern int otel_observer_u64_observe(otel_observer_u64_t*, uint64_t, const void*, size_t);
extern void otel_counter_u64_destroy(otel_counter_u64_t*);
extern void otel_gauge_f64_destroy(otel_gauge_f64_t*);
extern void otel_histogram_f64_destroy(otel_histogram_f64_t*);
extern void otel_observable_gauge_u64_destroy(otel_observable_gauge_u64_t*);
extern void otel_meter_destroy(otel_meter_t*);
extern void otel_meter_provider_destroy(otel_meter_provider_t*);
typedef struct otel_trace_exporter_t otel_trace_exporter_t;
typedef struct otel_span_processor_t otel_span_processor_t;
typedef struct otel_otlp_trace_exporter_builder_t otel_otlp_trace_exporter_builder_t;
typedef struct otel_batch_span_processor_builder_t otel_batch_span_processor_builder_t;
typedef struct otel_metric_exporter_t otel_metric_exporter_t;
typedef struct otel_otlp_metric_exporter_builder_t otel_otlp_metric_exporter_builder_t;
typedef struct otel_periodic_metric_reader_builder_t otel_periodic_metric_reader_builder_t;
typedef struct otel_periodic_metric_reader_t otel_periodic_metric_reader_t;
extern otel_otlp_trace_exporter_builder_t* otel_otlp_trace_exporter_builder_new(void);
extern int otel_otlp_trace_exporter_builder_set_endpoint(otel_otlp_trace_exporter_builder_t*, otel_string_view_t);
extern int otel_otlp_trace_exporter_builder_set_timeout_millis(otel_otlp_trace_exporter_builder_t*, uint64_t);
extern int otel_otlp_trace_exporter_builder_build(const otel_otlp_trace_exporter_builder_t*, otel_trace_exporter_t**);
extern void otel_otlp_trace_exporter_builder_destroy(otel_otlp_trace_exporter_builder_t*);
extern void otel_trace_exporter_destroy(otel_trace_exporter_t*);
extern otel_batch_span_processor_builder_t* otel_batch_span_processor_builder_new(void);
extern int otel_batch_span_processor_builder_set_exporter(otel_batch_span_processor_builder_t*, otel_trace_exporter_t*);
extern int otel_batch_span_processor_builder_set_max_queue_size(otel_batch_span_processor_builder_t*, size_t);
extern int otel_batch_span_processor_builder_build(otel_batch_span_processor_builder_t*, otel_span_processor_t**);
extern void otel_batch_span_processor_builder_destroy(otel_batch_span_processor_builder_t*);
extern void otel_span_processor_destroy(otel_span_processor_t*);
extern otel_otlp_metric_exporter_builder_t* otel_otlp_metric_exporter_builder_new(void);
extern int otel_otlp_metric_exporter_builder_set_endpoint(otel_otlp_metric_exporter_builder_t*, otel_string_view_t);
extern int otel_otlp_metric_exporter_builder_set_transport(otel_otlp_metric_exporter_builder_t*, uint32_t);
extern int otel_otlp_metric_exporter_builder_set_compression(
    otel_otlp_metric_exporter_builder_t*, uint32_t);
extern int otel_otlp_metric_exporter_builder_set_timeout_millis(
    otel_otlp_metric_exporter_builder_t*, uint64_t);
extern int otel_otlp_metric_exporter_builder_add_header(
    otel_otlp_metric_exporter_builder_t*, otel_string_view_t, otel_string_view_t);
extern int otel_otlp_metric_exporter_builder_build(const otel_otlp_metric_exporter_builder_t*, otel_metric_exporter_t**);
extern void otel_otlp_metric_exporter_builder_destroy(otel_otlp_metric_exporter_builder_t*);
extern void otel_metric_exporter_destroy(otel_metric_exporter_t*);
extern otel_periodic_metric_reader_builder_t* otel_periodic_metric_reader_builder_new(void);
extern int otel_periodic_metric_reader_builder_set_exporter(otel_periodic_metric_reader_builder_t*, otel_metric_exporter_t*);
extern int otel_periodic_metric_reader_builder_build(otel_periodic_metric_reader_builder_t*, otel_periodic_metric_reader_t**);
extern void otel_periodic_metric_reader_builder_destroy(otel_periodic_metric_reader_builder_t*);
extern void otel_periodic_metric_reader_destroy(otel_periodic_metric_reader_t*);
extern otel_sdk_builder_t* otel_sdk_builder_new(void);
extern int otel_sdk_builder_set_service_name(otel_sdk_builder_t*, otel_string_view_t);
extern int otel_sdk_builder_add_resource_attribute(otel_sdk_builder_t*, otel_key_value_t);
extern int otel_sdk_builder_add_span_processor(otel_sdk_builder_t*, otel_span_processor_t*);
extern int otel_sdk_builder_add_metric_reader(otel_sdk_builder_t*, otel_periodic_metric_reader_t*);
extern int otel_sdk_build(otel_sdk_builder_t*, otel_sdk_t**);
extern void otel_sdk_builder_destroy(otel_sdk_builder_t*);
extern int otel_sdk_set_as_global(otel_sdk_t*);
extern int otel_sdk_set_metrics_as_global(otel_sdk_t*);
extern int otel_sdk_force_flush(otel_sdk_t*, uint64_t);
extern int otel_sdk_metrics_force_flush(otel_sdk_t*, uint64_t);
extern int otel_sdk_metrics_shutdown(otel_sdk_t*, uint64_t);
extern int otel_sdk_shutdown(otel_sdk_t*, uint64_t);
extern void otel_sdk_destroy(otel_sdk_t*);
static otel_string_view_t cs(const char* s){ otel_string_view_t v; v.ptr=s; v.len=s?strlen(s):0; return v; }
static otel_string_view_t emp(void){ otel_string_view_t v; v.ptr=(void*)0; v.len=0; return v; }
extern char* getenv(const char*);
static int observable_calls=0;
static int observable_destroyed=0;
static otel_observable_gauge_u64_t* observable=(void*)0;
static void observe_queue(otel_observer_u64_t* observer, void* user_data){
    (void)user_data;
    otel_key_value_t attr;
    attr.key=cs("route");
    attr.value_type=0;
    attr.value.string_value=cs("checkout");
    observable_calls++;
    otel_observer_u64_observe(observer,17,&attr,1);
}
static void destroy_observable_state(void* user_data){
    observable_destroyed++;
    free(user_data);
}
static void work(void){
    otel_tracer_provider_t* p = otel_global_tracer_provider();
    otel_tracer_t* t = otel_tracer_provider_get_tracer(p, cs("instr"), cs("1.0"), emp());
    otel_span_t* parent = otel_tracer_start_span(t, cs("parent"), (void*)0);
    otel_span_set_string_attribute(parent, cs("k"), cs("v"));
    otel_span_start_options_t o; o.kind=2; o.parent=parent;
    otel_span_t* child = otel_tracer_start_span(t, cs("child"), &o);
    otel_span_end(child); otel_span_destroy(child);
    otel_span_end(parent); otel_span_destroy(parent);
    otel_tracer_destroy(t); otel_tracer_provider_destroy(p);
}
static int metrics_work(void){
    int result=0;
    otel_meter_provider_t* p=(void*)0;
    otel_meter_t* m=(void*)0;
    otel_counter_u64_t* c=(void*)0;
    otel_gauge_f64_t* g=(void*)0;
    otel_histogram_f64_t* h=(void*)0;
    double boundaries[2]={5.0,10.0};
    otel_instrument_options_t counter_options={
        sizeof(otel_instrument_options_t),cs("completed requests"),cs("{request}"),(void*)0,0
    };
    otel_instrument_options_t gauge_options={
        sizeof(otel_instrument_options_t),cs("queued work"),cs("{item}"),(void*)0,0
    };
    otel_instrument_options_t histogram_options={
        sizeof(otel_instrument_options_t),cs("request duration"),cs("ms"),boundaries,2
    };
    otel_key_value_t attr;
    attr.key=cs("route");
    attr.value_type=0;
    attr.value.string_value=cs("checkout");

    p=otel_global_meter_provider();
    if (!p){ result=1; goto cleanup; }
    m=otel_meter_provider_get_meter(
        p,cs("metric-instr"),cs("1.2.3"),cs("https://schema.example/metrics/1.0"));
    if (!m){ result=2; goto cleanup; }
    if (otel_meter_create_u64_counter(m,cs("requests"),&counter_options,&c)!=0||!c){
        result=3; goto cleanup;
    }
    if (otel_meter_create_f64_gauge(m,cs("queue_depth"),&gauge_options,&g)!=0||!g){
        result=4; goto cleanup;
    }
    if (otel_meter_create_f64_histogram(m,cs("duration"),&histogram_options,&h)!=0||!h){
        result=5; goto cleanup;
    }
    if (otel_counter_u64_add(c,3,&attr,1)!=0){ result=6; goto cleanup; }
    if (otel_gauge_f64_record(g,2.5,&attr,1)!=0){ result=7; goto cleanup; }
    if (otel_histogram_f64_record(h,7.5,&attr,1)!=0){ result=8; goto cleanup; }

cleanup:
    if (h) otel_histogram_f64_destroy(h);
    if (g) otel_gauge_f64_destroy(g);
    if (c) otel_counter_u64_destroy(c);
    if (m) otel_meter_destroy(m);
    if (p) otel_meter_provider_destroy(p);
    return result;
}
static int observable_setup(void){
    int result=0;
    int create_status=0;
    otel_meter_provider_t* p=(void*)0;
    otel_meter_t* m=(void*)0;
    otel_observable_gauge_u64_t* created=(void*)0;
    void* state=(void*)0;
    otel_instrument_options_t options={
        sizeof(otel_instrument_options_t),cs("observable queue"),cs("{item}"),(void*)0,0
    };

    p=otel_global_meter_provider();
    if (!p){ result=1; goto cleanup; }
    m=otel_meter_provider_get_meter(
        p,cs("observable-instr"),cs("2.0"),cs("https://schema.example/observable/1.0"));
    if (!m){ result=2; goto cleanup; }
    state=malloc(1);
    if (!state){ result=3; goto cleanup; }

    create_status=otel_meter_create_u64_observable_gauge(
        m,cs("observable_queue"),&options,observe_queue,state,
        destroy_observable_state,&created);
    /*
     * The non-NULL out/callback/meter and fixed valid name/options above satisfy all
     * API-side validation. CallbackState therefore owns state before SDK dispatch,
     * including SDK-side creation failure.
     */
    state=(void*)0;
    if (create_status!=0){ result=4; goto cleanup; }
    if (!created){ result=5; goto cleanup; }
    observable=created;
    created=(void*)0;

cleanup:
    if (created) otel_observable_gauge_u64_destroy(created);
    if (state) free(state);
    if (m) otel_meter_destroy(m);
    if (p) otel_meter_provider_destroy(p);
    return result;
}
int main(void){
    int result=1;
    int stage_status=0;
    otel_otlp_trace_exporter_builder_t* eb=(void*)0;
    otel_trace_exporter_t* exporter=(void*)0;
    otel_batch_span_processor_builder_t* pb=(void*)0;
    otel_span_processor_t* processor=(void*)0;
    otel_sdk_builder_t* b=(void*)0;
    otel_otlp_metric_exporter_builder_t* meb=(void*)0;
    otel_metric_exporter_t* mex=(void*)0;
    otel_periodic_metric_reader_builder_t* mrb=(void*)0;
    otel_periodic_metric_reader_t* mr=(void*)0;
    otel_sdk_t* sdk=(void*)0;

    work(); /* API-only no-op before install (must be safe) */
    stage_status=metrics_work();
    if (stage_status!=0){ result=30+stage_status; goto cleanup; }
    /* Build the pipeline: OTLP exporter -> batch processor -> SDK builder. */
    eb=otel_otlp_trace_exporter_builder_new();
    if (!eb){ result=2; goto cleanup; }
    if (otel_otlp_trace_exporter_builder_set_endpoint(
            eb,cs(getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")))!=0){
        result=3; goto cleanup;
    }
    if (otel_otlp_trace_exporter_builder_set_timeout_millis(eb,5000)!=0){
        result=4; goto cleanup;
    }
    if (otel_otlp_trace_exporter_builder_build(eb,&exporter)!=0||!exporter){
        result=5; goto cleanup;
    }
    otel_otlp_trace_exporter_builder_destroy(eb);
    eb=(void*)0;
    pb=otel_batch_span_processor_builder_new();
    if (!pb){ result=6; goto cleanup; }
    if (otel_batch_span_processor_builder_set_exporter(pb,exporter)!=0){
        result=7; goto cleanup;
    }
    exporter=(void*)0;
    if (otel_batch_span_processor_builder_build(pb,&processor)!=0||!processor){
        result=8; goto cleanup;
    }
    otel_batch_span_processor_builder_destroy(pb);
    pb=(void*)0;
    b=otel_sdk_builder_new();
    if (!b){ result=9; goto cleanup; }
    if (otel_sdk_builder_set_service_name(b,cs("cross-artifact"))!=0){
        result=10; goto cleanup;
    }
    {
        otel_key_value_t attr;
        attr.key=cs("deployment.environment");
        attr.value_type=0;
        attr.value.string_value=cs("integration");
        if (otel_sdk_builder_add_resource_attribute(b,attr)!=0){
            result=74; goto cleanup;
        }
    }
    if (otel_sdk_builder_add_span_processor(b,processor)!=0){
        result=11; goto cleanup;
    }
    processor=(void*)0;
    meb=otel_otlp_metric_exporter_builder_new();
    if (!meb){ result=12; goto cleanup; }
    if (otel_otlp_metric_exporter_builder_set_endpoint(
            meb,cs(getenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")))!=0){
        result=13; goto cleanup;
    }
    if (otel_otlp_metric_exporter_builder_set_timeout_millis(meb,5000)!=0){
        result=78; goto cleanup;
    }
    if (getenv("OTEL_TEST_METRICS_GRPC") &&
        otel_otlp_metric_exporter_builder_set_transport(meb,1)!=0){
        result=75; goto cleanup;
    }
    if (getenv("OTEL_TEST_METRICS_COMPRESSION") &&
        otel_otlp_metric_exporter_builder_set_compression(
            meb,(uint32_t)atoi(getenv("OTEL_TEST_METRICS_COMPRESSION")))!=0){
        result=77; goto cleanup;
    }
    if (getenv("OTEL_TEST_METRICS_GRPC") &&
        otel_otlp_metric_exporter_builder_add_header(
            meb,cs("x-tenant"),cs("integration"))!=0){
        result=76; goto cleanup;
    }
    if (otel_otlp_metric_exporter_builder_build(meb,&mex)!=0||!mex){
        result=14; goto cleanup;
    }
    otel_otlp_metric_exporter_builder_destroy(meb);
    meb=(void*)0;
    mrb=otel_periodic_metric_reader_builder_new();
    if (!mrb){ result=15; goto cleanup; }
    if (otel_periodic_metric_reader_builder_set_exporter(mrb,mex)!=0){
        result=16; goto cleanup;
    }
    mex=(void*)0;
    if (otel_periodic_metric_reader_builder_build(mrb,&mr)!=0||!mr){
        result=17; goto cleanup;
    }
    otel_periodic_metric_reader_builder_destroy(mrb);
    mrb=(void*)0;
    if (otel_sdk_builder_add_metric_reader(b,mr)!=0){
        result=18; goto cleanup;
    }
    mr=(void*)0;
    if (otel_sdk_build(b,&sdk)!=0||!sdk){ result=19; goto cleanup; }
    otel_sdk_builder_destroy(b);
    b=(void*)0;
    if (otel_sdk_set_as_global(sdk)!=0){ result=20; goto cleanup; }
    if (otel_sdk_set_metrics_as_global(sdk)!=0){ result=21; goto cleanup; }
    work(); /* API-only calls AFTER install must export through the SDK */
    stage_status=metrics_work();
    if (stage_status!=0){ result=40+stage_status; goto cleanup; }
    stage_status=observable_setup();
    if (stage_status!=0){ result=60+stage_status; goto cleanup; }
    if (otel_sdk_force_flush(sdk,5000)!=0){ result=70; goto cleanup; }
    if (otel_sdk_metrics_force_flush(sdk,0)!=0){ result=71; goto cleanup; }
    if (observable_calls==0){ result=72; goto cleanup; }
    result=0;

cleanup:
    if (observable){
        otel_observable_gauge_u64_destroy(observable);
        observable=(void*)0;
    }
    if (sdk){
        if (getenv("OTEL_TEST_SKIP_METRICS_SHUTDOWN")==NULL)
            otel_sdk_metrics_shutdown(sdk,5000);
        otel_sdk_shutdown(sdk,5000);
        otel_sdk_destroy(sdk);
    }
    if (b) otel_sdk_builder_destroy(b);
    if (mr) otel_periodic_metric_reader_destroy(mr);
    if (mrb) otel_periodic_metric_reader_builder_destroy(mrb);
    if (mex) otel_metric_exporter_destroy(mex);
    if (meb) otel_otlp_metric_exporter_builder_destroy(meb);
    if (processor) otel_span_processor_destroy(processor);
    if (pb) otel_batch_span_processor_builder_destroy(pb);
    if (exporter) otel_trace_exporter_destroy(exporter);
    if (eb) otel_otlp_trace_exporter_builder_destroy(eb);
    if (result==0&&observable_destroyed!=1) return 73;
    return result;
}
"#;

struct MockCollector {
    port: u16,
    bytes: Arc<AtomicUsize>,
    metric_bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

/// Minimal mock OTLP/HTTP collector: accepts POSTs and retains complete Metrics bodies.
fn start_mock() -> MockCollector {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let bytes = Arc::new(AtomicUsize::new(0));
    let metric_bodies = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (b2, bodies2, s2) = (
        Arc::clone(&bytes),
        Arc::clone(&metric_bodies),
        Arc::clone(&stop),
    );
    let thread = std::thread::spawn(move || {
        while !s2.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    // Read headers to find Content-Length, then the body.
                    let mut content_len = None;
                    let mut header_end = None;
                    loop {
                        match sock.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&tmp[..n]);
                                if header_end.is_none() {
                                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                                    {
                                        header_end = Some(pos + 4);
                                        let headers =
                                            String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                                        for line in headers.lines() {
                                            if let Some(v) = line.strip_prefix("content-length:") {
                                                content_len = v.trim().parse().ok();
                                            }
                                        }
                                    }
                                }
                                if let (Some(he), Some(content_len)) = (header_end, content_len) {
                                    if buf.len().saturating_sub(he) >= content_len {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    if let (Some(he), Some(content_len)) = (header_end, content_len) {
                        if buf.len().saturating_sub(he) >= content_len {
                            b2.fetch_add(content_len, Ordering::Relaxed);
                            if String::from_utf8_lossy(&buf[..he]).contains("POST /v1/metrics") {
                                bodies2
                                    .lock()
                                    .unwrap()
                                    .push(buf[he..he + content_len].to_vec());
                            }
                        }
                    }
                    let _ = sock.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\n\r\n",
                    );
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    MockCollector {
        port,
        bytes,
        metric_bodies,
        stop,
        thread,
    }
}

#[cfg(feature = "otlp-grpc")]
#[derive(Clone)]
struct GrpcMetricsService {
    requests: Arc<(Mutex<Vec<ExportMetricsServiceRequest>>, std::sync::Condvar)>,
    metadata_verified: Arc<AtomicBool>,
}

#[cfg(feature = "otlp-grpc")]
#[tonic::async_trait]
impl MetricsService for GrpcMetricsService {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        if request
            .metadata()
            .get("x-tenant")
            .and_then(|value| value.to_str().ok())
            == Some("integration")
        {
            self.metadata_verified.store(true, Ordering::Release);
        }
        let (requests, arrived) = &*self.requests;
        requests.lock().unwrap().push(request.into_inner());
        arrived.notify_all();
        Ok(Response::new(ExportMetricsServiceResponse::default()))
    }
}

#[cfg(feature = "otlp-grpc")]
struct GrpcCollector {
    port: u16,
    requests: Arc<(Mutex<Vec<ExportMetricsServiceRequest>>, std::sync::Condvar)>,
    metadata_verified: Arc<AtomicBool>,
    shutdown: oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(feature = "otlp-grpc")]
fn start_grpc_collector() -> GrpcCollector {
    let requests = Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));
    let server_requests = Arc::clone(&requests);
    let metadata_verified = Arc::new(AtomicBool::new(false));
    let server_metadata_verified = Arc::clone(&metadata_verified);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build gRPC collector runtime");
        runtime.block_on(async move {
            let listener = TokioTcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind gRPC collector");
            ready_tx
                .send(
                    listener
                        .local_addr()
                        .expect("gRPC collector address")
                        .port(),
                )
                .expect("publish gRPC collector port");
            let service = MetricsServiceServer::new(GrpcMetricsService {
                requests: server_requests,
                metadata_verified: server_metadata_verified,
            });
            #[cfg(feature = "otlp-grpc-gzip")]
            let service = service.accept_compressed(CompressionEncoding::Gzip);
            tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve gRPC collector");
        });
    });
    let port = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("gRPC collector startup timed out");
    GrpcCollector {
        port,
        requests,
        metadata_verified,
        shutdown: shutdown_tx,
        thread,
    }
}

fn has_string_attribute(attributes: &[KeyValue], key: &str, expected: &str) -> bool {
    attributes.iter().any(|attribute| {
        attribute.key == key
            && matches!(
                attribute.value.as_ref().and_then(|value| value.value.as_ref()),
                Some(any_value::Value::StringValue(value)) if value == expected
            )
    })
}

fn assert_decoded_metrics(bodies: &[Vec<u8>]) {
    let requests = bodies
        .iter()
        .map(|body| ExportMetricsServiceRequest::decode(body.as_slice()).expect("decode OTLP"))
        .collect::<Vec<_>>();
    assert_metric_requests(&requests);
}

fn assert_metric_requests(requests: &[ExportMetricsServiceRequest]) {
    let mut resource_verified = false;
    let mut scope_verified = false;
    let mut counter_verified = false;
    let mut gauge_verified = false;
    let mut histogram_verified = false;
    let mut observable_verified = false;

    for request in requests {
        for resource_metrics in &request.resource_metrics {
            if let Some(resource) = &resource_metrics.resource {
                resource_verified |=
                    has_string_attribute(&resource.attributes, "service.name", "cross-artifact")
                        && has_string_attribute(
                            &resource.attributes,
                            "deployment.environment",
                            "integration",
                        );
            }
            for scope_metrics in &resource_metrics.scope_metrics {
                if let Some(scope) = &scope_metrics.scope {
                    if scope.name == "metric-instr" {
                        scope_verified |= scope.version == "1.2.3"
                            && scope_metrics.schema_url == "https://schema.example/metrics/1.0";
                    }
                }
                for metric in &scope_metrics.metrics {
                    match (&*metric.name, metric.data.as_ref()) {
                        ("requests", Some(metric::Data::Sum(sum))) => {
                            counter_verified |= metric.description == "completed requests"
                                && metric.unit == "{request}"
                                && sum.is_monotonic
                                && sum.data_points.iter().any(|point| {
                                    matches!(point.value, Some(number_data_point::Value::AsInt(3)))
                                        && has_string_attribute(
                                            &point.attributes,
                                            "route",
                                            "checkout",
                                        )
                                });
                        }
                        ("queue_depth", Some(metric::Data::Gauge(gauge))) => {
                            gauge_verified |= metric.description == "queued work"
                                && metric.unit == "{item}"
                                && gauge.data_points.iter().any(|point| {
                                    matches!(
                                        point.value,
                                        Some(number_data_point::Value::AsDouble(value))
                                            if value == 2.5
                                    ) && has_string_attribute(
                                        &point.attributes,
                                        "route",
                                        "checkout",
                                    )
                                });
                        }
                        ("duration", Some(metric::Data::Histogram(histogram))) => {
                            histogram_verified |= metric.description == "request duration"
                                && metric.unit == "ms"
                                && histogram.data_points.iter().any(|point| {
                                    point.count == 1
                                        && point.sum == Some(7.5)
                                        && point.explicit_bounds == [5.0, 10.0]
                                        && point.bucket_counts == [0, 1, 0]
                                        && has_string_attribute(
                                            &point.attributes,
                                            "route",
                                            "checkout",
                                        )
                                });
                        }
                        ("observable_queue", Some(metric::Data::Gauge(gauge))) => {
                            observable_verified |= metric.description == "observable queue"
                                && metric.unit == "{item}"
                                && gauge.data_points.iter().any(|point| {
                                    matches!(point.value, Some(number_data_point::Value::AsInt(17)))
                                        && has_string_attribute(
                                            &point.attributes,
                                            "route",
                                            "checkout",
                                        )
                                });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    assert!(resource_verified, "resource attributes were not exported");
    assert!(
        scope_verified,
        "instrumentation scope name, version, or schema URL was not exported"
    );
    assert!(
        counter_verified,
        "counter metadata/value/attributes missing"
    );
    assert!(gauge_verified, "gauge metadata/value/attributes missing");
    assert!(
        histogram_verified,
        "histogram metadata/value/boundaries/attributes missing"
    );
    assert!(
        observable_verified,
        "observable metric metadata/value/attributes missing"
    );
}

#[test]
fn api_only_calls_after_sdk_install_export_through_sdk() {
    // This proof relies on Unix dynamic-linking semantics (rpath plus DYLD_LIBRARY_PATH /
    // LD_LIBRARY_PATH). Windows dynamic linking of the split is not a supported/claimed
    // model, so skip cleanly on non-Unix targets — even under CI — rather than fail
    // confusingly. Unix CI fail-hard behavior (missing cc / cdylibs) is unchanged.
    if !cfg!(unix) {
        eprintln!(
            "skipping: the cross-artifact proof requires Unix dynamic linking (non-Unix target)"
        );
        return;
    }
    let cc = match find_cc() {
        Some(cc) => cc,
        None => {
            if is_ci() {
                panic!(
                    "CI=true but no C compiler was found: the cross-artifact global-provider \
                     proof cannot run. Install a C compiler or set the CC environment variable."
                );
            }
            eprintln!("skipping: no C compiler (set CC to enable)");
            return;
        }
    };
    let lib_dir = match find_lib_dir() {
        Some(d) => d,
        None => {
            if is_ci() {
                panic!(
                    "CI=true but the cdylibs are not built: the cross-artifact global-provider \
                     proof cannot run. Build them first with: \
                     `cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk`."
                );
            }
            eprintln!(
                "skipping: cdylibs not built. Run: cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk"
            );
            return;
        }
    };

    // Unique per process (and run) so concurrent `cargo test` invocations do not collide on the
    // harness source/binary in the shared temp dir. `src` derives from `out`, so both are unique.
    let unique = format!(
        "otel_c_cross_artifact_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let out = std::env::temp_dir().join(unique);
    let src = out.with_extension("c");
    std::fs::write(&src, HARNESS_C).expect("write harness");

    let mut cmd = Command::new(&cc);
    cmd.arg("-std=c11")
        .arg(&src)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lopentelemetry_c_api")
        .arg("-lopentelemetry_c_sdk")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&out);
    let compile = cmd.output().expect("invoke cc");
    assert!(
        compile.status.success(),
        "harness failed to compile/link:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let collector = start_mock();
    let endpoint = format!("http://127.0.0.1:{}/v1/traces", collector.port);
    let metrics_endpoint = format!("http://127.0.0.1:{}/v1/metrics", collector.port);
    let run = Command::new(&out)
        .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", &endpoint)
        .env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", &metrics_endpoint)
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .expect("run harness");
    // Wait (bounded) for the collector to receive the export. This avoids a fixed sleep that can
    // stop the mock too early under slow CI while still failing promptly if no POST arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while collector.bytes.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    collector.stop.store(true, Ordering::Release);
    collector.thread.join().expect("join mock collector");

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(
        run.status.success(),
        "harness exited with failure ({:?}):\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let received = collector.bytes.load(Ordering::Relaxed);
    assert!(
        received > 0,
        "the mock collector received no exported span bytes — API-only calls after SDK \
         install did NOT reach the SDK across the artifact boundary"
    );
    let metric_bodies = collector.metric_bodies.lock().unwrap();
    assert!(
        !metric_bodies.is_empty(),
        "the mock collector received no OTLP metric requests through the API-owned Metrics slot"
    );
    assert_decoded_metrics(&metric_bodies);
    eprintln!("cross-artifact export OK: {received} protobuf bytes via API-only path");
}

#[cfg(feature = "otlp-grpc")]
#[test]
fn c_application_exports_metrics_through_grpc_without_tokio_runtime() {
    if !cfg!(unix) {
        return;
    }
    let cc = find_cc().expect("a C compiler is required for the gRPC cross-artifact test");
    let lib_dir = find_lib_dir()
        .expect("cdylibs must be built with otlp-grpc before the gRPC cross-artifact test runs");
    let unique = format!(
        "otel_c_cross_artifact_grpc_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let out = std::env::temp_dir().join(unique);
    let src = out.with_extension("c");
    std::fs::write(&src, HARNESS_C).expect("write gRPC harness");

    let compile = Command::new(&cc)
        .arg("-std=c11")
        .arg(&src)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lopentelemetry_c_api")
        .arg("-lopentelemetry_c_sdk")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&out)
        .output()
        .expect("compile gRPC harness");
    assert!(
        compile.status.success(),
        "gRPC harness failed to compile/link:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let trace_collector = start_mock();
    let metrics_collector = start_grpc_collector();
    let trace_endpoint = format!("http://127.0.0.1:{}/v1/traces", trace_collector.port);
    let metrics_endpoint = format!("http://127.0.0.1:{}", metrics_collector.port);
    let mut command = Command::new(&out);
    command
        .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", &trace_endpoint)
        .env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", &metrics_endpoint)
        .env("OTEL_TEST_METRICS_GRPC", "1")
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir);
    let run = output_with_timeout(&mut command, Duration::from_secs(10));

    let (request_mutex, arrived) = &*metrics_collector.requests;
    let (requests, first_wait) = arrived
        .wait_timeout_while(
            request_mutex.lock().unwrap(),
            Duration::from_secs(5),
            |requests| requests.is_empty(),
        )
        .expect("wait for gRPC Metrics request");
    assert!(!first_wait.timed_out(), "gRPC Metrics export timed out");
    drop(requests);

    let mut command_without_shutdown = Command::new(&out);
    command_without_shutdown
        .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", &trace_endpoint)
        .env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", &metrics_endpoint)
        .env("OTEL_TEST_METRICS_GRPC", "1")
        .env("OTEL_TEST_SKIP_METRICS_SHUTDOWN", "1")
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir);
    let run_without_shutdown =
        output_with_timeout(&mut command_without_shutdown, Duration::from_secs(10));
    let (requests, second_wait) = arrived
        .wait_timeout_while(
            request_mutex.lock().unwrap(),
            Duration::from_secs(5),
            |requests| requests.len() < 2,
        )
        .expect("wait for second gRPC Metrics request");
    assert!(
        !second_wait.timed_out(),
        "gRPC Metrics export during SDK destruction timed out"
    );
    drop(requests);

    #[cfg(feature = "otlp-grpc-gzip")]
    let compressed_run = {
        let mut compressed_command = Command::new(&out);
        compressed_command
            .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", &trace_endpoint)
            .env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", &metrics_endpoint)
            .env("OTEL_TEST_METRICS_GRPC", "1")
            .env("OTEL_TEST_METRICS_COMPRESSION", "1")
            .env("DYLD_LIBRARY_PATH", &lib_dir)
            .env("LD_LIBRARY_PATH", &lib_dir);
        let run = output_with_timeout(&mut compressed_command, Duration::from_secs(10));
        let (requests, compressed_wait) = arrived
            .wait_timeout_while(
                request_mutex.lock().unwrap(),
                Duration::from_secs(5),
                |requests| requests.len() < 3,
            )
            .expect("wait for compressed gRPC Metrics request");
        assert!(
            !compressed_wait.timed_out(),
            "gzip gRPC Metrics export timed out"
        );
        drop(requests);
        run
    };
    let requests = request_mutex.lock().unwrap().clone();
    assert!(
        metrics_collector.metadata_verified.load(Ordering::Acquire),
        "configured gRPC metadata did not reach the collector"
    );

    trace_collector.stop.store(true, Ordering::Release);
    trace_collector.thread.join().expect("join trace collector");
    metrics_collector
        .shutdown
        .send(())
        .expect("request gRPC collector shutdown");
    metrics_collector
        .thread
        .join()
        .expect("join gRPC collector");

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(
        run.status.success(),
        "gRPC harness exited with failure ({:?}):\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    assert!(
        run_without_shutdown.status.success(),
        "gRPC harness without explicit shutdown failed ({:?}):\nstdout: {}\nstderr: {}",
        run_without_shutdown.status.code(),
        String::from_utf8_lossy(&run_without_shutdown.stdout),
        String::from_utf8_lossy(&run_without_shutdown.stderr),
    );
    #[cfg(feature = "otlp-grpc-gzip")]
    assert!(
        compressed_run.status.success(),
        "gzip gRPC harness failed ({:?}):\nstdout: {}\nstderr: {}",
        compressed_run.status.code(),
        String::from_utf8_lossy(&compressed_run.stdout),
        String::from_utf8_lossy(&compressed_run.stderr),
    );
    assert_metric_requests(&requests);
}
