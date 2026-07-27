//! Best-effort check that the SDK header (and the split example) compile with a system C
//! compiler (syntax-only). `sdk.h` includes the API's `common.h`/`trace.h`, so the API's
//! include directory is also on the search path — mirroring how an application compiles.
//! Self-skips if no compiler is available.

use std::path::PathBuf;
use std::process::Command;

fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    for c in ["cc", "clang", "gcc"] {
        if Command::new(c)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(c.to_owned());
        }
    }
    None
}

fn find_cxx() -> Option<String> {
    if let Ok(cxx) = std::env::var("CXX") {
        if !cxx.is_empty() {
            return Some(cxx);
        }
    }
    for compiler in ["c++", "clang++", "g++"] {
        if Command::new(compiler)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            return Some(compiler.to_owned());
        }
    }
    None
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn api_include() -> PathBuf {
    // The API crate is a sibling under `opentelemetry-c/`: sdk -> opentelemetry-c -> api.
    manifest().parent().unwrap().join("api/include")
}

fn sdk_include() -> PathBuf {
    manifest().join("include")
}

/// A unique temp `.c` path per invocation, so parallel test threads/processes never clobber
/// or delete each other's source file. std-only: process id + `SystemTime` nanos + a
/// monotonic per-process counter.
fn unique_temp_c(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "otel_c_{label}_hdr_check_{}_{}_{}.c",
        std::process::id(),
        nanos,
        seq
    ))
}

fn syntax_check(cc: &str, args: &[&std::ffi::OsStr]) {
    let out = Command::new(cc).args(args).output().expect("invoke cc");
    assert!(
        out.status.success(),
        "compile failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn sdk_header_and_example_compile() {
    let cc = match find_cc() {
        Some(cc) => cc,
        None => {
            eprintln!("skipping: no C compiler found");
            return;
        }
    };
    let api_inc = api_include();
    let sdk_inc = sdk_include();

    // A TU that includes only sdk.h (which pulls in the API's common.h/trace.h), and also
    // exercises the optional header-only helpers to confirm they are reachable through the
    // SDK header context. `-fsyntax-only` does not link, so a NULL span is fine.
    let tmp = unique_temp_c("sdk");
    std::fs::write(
        &tmp,
        r#"#include <opentelemetry_c/sdk.h>
int main(void) {
    otel_sdk_builder_t* b = otel_sdk_builder_new();
    (void)b;
    otel_span_t* span = (void*)0;
    (void)otel_span_set_attribute(span, otel_kv_double(otel_cstr("d"), 2.5));
    (void)otel_span_set_ok(span);
    return 0;
}
"#,
    )
    .expect("write temp source");
    syntax_check(
        &cc,
        &[
            "-std=c11".as_ref(),
            "-Wall".as_ref(),
            "-Wextra".as_ref(),
            "-Werror".as_ref(),
            "-fsyntax-only".as_ref(),
            "-I".as_ref(),
            api_inc.as_os_str(),
            "-I".as_ref(),
            sdk_inc.as_os_str(),
            tmp.as_os_str(),
        ],
    );
    let _ = std::fs::remove_file(&tmp);

    // A TU that includes every pipeline header and exercises the full builder chain, so each
    // new header compiles standalone and the ownership-transfer signatures line up.
    let pipeline = unique_temp_c("pipeline");
    std::fs::write(
        &pipeline,
        r#"#include <opentelemetry_c/sdk.h>
#include <opentelemetry_c/trace_exporter.h>
#include <opentelemetry_c/span_processor.h>
#include <opentelemetry_c/otlp_trace_exporter.h>
#include <opentelemetry_c/batch_span_processor.h>
#include <opentelemetry_c/otlp_metric_exporter.h>
#include <opentelemetry_c/periodic_metric_reader.h>
#include <opentelemetry_c/metric_view.h>
#include <opentelemetry_c/log_exporter.h>
#include <opentelemetry_c/log_processor.h>
#include <opentelemetry_c/otlp_log_exporter.h>
int main(void) {
    otel_otlp_trace_exporter_builder_t* eb = otel_otlp_trace_exporter_builder_new();
    otel_otlp_trace_exporter_builder_set_endpoint(eb, otel_cstr("http://localhost:4318/v1/traces"));
    otel_otlp_trace_exporter_builder_set_timeout_millis(eb, 5000);
    otel_trace_exporter_t* exporter = NULL;
    otel_otlp_trace_exporter_builder_build(eb, &exporter);
    otel_otlp_trace_exporter_builder_destroy(eb);

    otel_batch_span_processor_builder_t* pb = otel_batch_span_processor_builder_new();
    otel_batch_span_processor_builder_set_exporter(pb, exporter);
    otel_batch_span_processor_builder_set_max_queue_size(pb, 2048);
    otel_span_processor_t* processor = NULL;
    otel_batch_span_processor_builder_build(pb, &processor);
    otel_batch_span_processor_builder_destroy(pb);

    otel_sdk_builder_t* sb = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(sb, otel_cstr("hdr-check"));
    otel_sdk_builder_add_span_processor(sb, processor);

    otel_otlp_metric_exporter_builder_t* meb = otel_otlp_metric_exporter_builder_new();
    otel_otlp_metric_exporter_builder_set_endpoint(meb, otel_cstr("http://localhost:4318/v1/metrics"));
    otel_otlp_metric_exporter_builder_set_transport(
        meb, OTEL_OTLP_METRIC_TRANSPORT_HTTP_PROTOBUF);
    otel_otlp_metric_exporter_builder_set_compression(meb, OTEL_OTLP_COMPRESSION_NONE);
    otel_metric_exporter_t* metric_exporter = NULL;
    otel_otlp_metric_exporter_builder_build(meb, &metric_exporter);
    otel_otlp_metric_exporter_builder_destroy(meb);
    otel_periodic_metric_reader_builder_t* mrb = otel_periodic_metric_reader_builder_new();
    otel_periodic_metric_reader_builder_set_exporter(mrb, metric_exporter);
    otel_periodic_metric_reader_t* reader = NULL;
    otel_periodic_metric_reader_builder_build(mrb, &reader);
    otel_periodic_metric_reader_builder_destroy(mrb);
    otel_sdk_builder_add_metric_reader(sb, reader);

    otel_metric_view_builder_t* vb = otel_metric_view_builder_new();
    otel_metric_view_builder_set_name_pattern(vb, otel_cstr("request.*"));
    otel_metric_view_builder_set_cardinality_limit(vb, 100);
    otel_metric_view_builder_set_attribute_filter_enabled(vb, 1);
    otel_metric_view_t* view = NULL;
    otel_metric_view_builder_build(vb, &view);
    otel_metric_view_builder_destroy(vb);
    otel_sdk_builder_add_metric_view(sb, view);

    /* Logs pipeline: OTLP exporter -> batch processor -> SDK builder. */
    otel_otlp_log_exporter_builder_t* leb = otel_otlp_log_exporter_builder_new();
    otel_otlp_log_exporter_builder_set_endpoint(leb, otel_cstr("http://localhost:4318/v1/logs"));
    otel_otlp_log_exporter_builder_set_transport(leb, OTEL_OTLP_LOG_TRANSPORT_HTTP_PROTOBUF);
    otel_otlp_log_exporter_builder_set_compression(leb, OTEL_OTLP_COMPRESSION_NONE);
    otel_otlp_log_exporter_builder_add_header(leb, otel_cstr("api-key"), otel_cstr("secret"));
    otel_otlp_log_exporter_builder_set_timeout_millis(leb, 5000);
    otel_log_exporter_t* log_exporter = NULL;
    otel_otlp_log_exporter_builder_build(leb, &log_exporter);
    otel_otlp_log_exporter_builder_destroy(leb);

    otel_batch_log_processor_builder_t* lpb = otel_batch_log_processor_builder_new();
    otel_batch_log_processor_builder_set_exporter(lpb, log_exporter);
    otel_batch_log_processor_builder_set_max_queue_size(lpb, 2048);
    otel_batch_log_processor_builder_set_max_export_batch_size(lpb, 512);
    otel_batch_log_processor_builder_set_scheduled_delay_millis(lpb, 1000);
    otel_batch_log_processor_builder_set_max_export_timeout_millis(lpb, 30000);
    otel_log_processor_t* log_processor = NULL;
    otel_batch_log_processor_builder_build(lpb, &log_processor);
    otel_batch_log_processor_builder_destroy(lpb);
    otel_sdk_builder_add_log_processor(sb, log_processor);

    otel_sdk_t* sdk = NULL;
    otel_sdk_build(sb, &sdk);
    otel_sdk_builder_destroy(sb);

    /* Logs lifecycle entry points. */
    otel_logger_provider_t* logger_provider = otel_sdk_get_logger_provider(sdk);
    otel_logger_provider_destroy(logger_provider);
    otel_sdk_set_logs_as_global(sdk);
    otel_sdk_logs_force_flush(sdk, 0);
    otel_sdk_logs_shutdown(sdk, 5000);
    (void)sdk;
    return 0;
}
"#,
    )
    .expect("write pipeline source");
    syntax_check(
        &cc,
        &[
            "-std=c11".as_ref(),
            "-Wall".as_ref(),
            "-Wextra".as_ref(),
            "-Werror".as_ref(),
            "-fsyntax-only".as_ref(),
            "-I".as_ref(),
            api_inc.as_os_str(),
            "-I".as_ref(),
            sdk_inc.as_os_str(),
            pipeline.as_os_str(),
        ],
    );

    if let Some(cxx) = find_cxx() {
        syntax_check(
            &cxx,
            &[
                "-std=c++17".as_ref(),
                "-Wall".as_ref(),
                "-Wextra".as_ref(),
                "-Werror".as_ref(),
                "-fsyntax-only".as_ref(),
                "-x".as_ref(),
                "c++".as_ref(),
                "-I".as_ref(),
                api_inc.as_os_str(),
                "-I".as_ref(),
                sdk_inc.as_os_str(),
                pipeline.as_os_str(),
            ],
        );
    } else {
        eprintln!("skipping C++ header check: no C++ compiler found");
    }
    let _ = std::fs::remove_file(&pipeline);

    for (label, header) in [
        ("metrics", "opentelemetry_c/metrics.h"),
        (
            "otlp_metric_exporter",
            "opentelemetry_c/otlp_metric_exporter.h",
        ),
        (
            "periodic_metric_reader",
            "opentelemetry_c/periodic_metric_reader.h",
        ),
        ("metric_view", "opentelemetry_c/metric_view.h"),
    ] {
        let standalone = unique_temp_c(label);
        std::fs::write(
            &standalone,
            format!("#include <{header}>\nint main(void) {{ return 0; }}\n"),
        )
        .expect("write standalone header source");
        syntax_check(
            &cc,
            &[
                "-std=c11".as_ref(),
                "-Wall".as_ref(),
                "-Wextra".as_ref(),
                "-Werror".as_ref(),
                "-fsyntax-only".as_ref(),
                "-I".as_ref(),
                api_inc.as_os_str(),
                "-I".as_ref(),
                sdk_inc.as_os_str(),
                standalone.as_os_str(),
            ],
        );
        let _ = std::fs::remove_file(&standalone);
    }

    let metrics_example = manifest().join("examples/c-metrics/main.c");
    assert!(
        metrics_example.exists(),
        "example missing: {}",
        metrics_example.display()
    );
    syntax_check(
        &cc,
        &[
            "-std=c11".as_ref(),
            "-Wall".as_ref(),
            "-Wextra".as_ref(),
            "-Werror".as_ref(),
            "-fsyntax-only".as_ref(),
            "-I".as_ref(),
            api_inc.as_os_str(),
            "-I".as_ref(),
            sdk_inc.as_os_str(),
            metrics_example.as_os_str(),
        ],
    );

    // The shipped split example (includes api.h + sdk.h).
    let example = manifest().join("examples/c-basic-traces/main.c");
    assert!(example.exists(), "example missing: {}", example.display());
    syntax_check(
        &cc,
        &[
            "-std=c11".as_ref(),
            "-Wall".as_ref(),
            "-Wextra".as_ref(),
            "-Werror".as_ref(),
            "-fsyntax-only".as_ref(),
            "-I".as_ref(),
            api_inc.as_os_str(),
            "-I".as_ref(),
            sdk_inc.as_os_str(),
            example.as_os_str(),
        ],
    );
}
