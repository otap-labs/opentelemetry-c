//! Best-effort check that the API library's public C headers compile with a system C
//! compiler (syntax-only, no linking). Self-skips if no compiler is available.

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

fn include_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("include")
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

fn syntax_check(cc: &str, include: &PathBuf, src: &str) {
    let tmp = unique_temp_c("api");
    std::fs::write(&tmp, src).expect("write temp source");
    let out = Command::new(cc)
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsyntax-only",
            "-I",
        ])
        .arg(include)
        .arg(&tmp)
        .output()
        .expect("invoke cc");
    let _ = std::fs::remove_file(&tmp);
    assert!(
        out.status.success(),
        "API header failed to compile:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn api_umbrella_header_compiles() {
    let cc = match find_cc() {
        Some(cc) => cc,
        None => {
            eprintln!("skipping: no C compiler found");
            return;
        }
    };
    syntax_check(
        &cc,
        &include_dir(),
        "#include <opentelemetry_c/api.h>\nint main(void){ return (int)otel_version_minor(); }\n",
    );
    // Individual headers too.
    syntax_check(
        &cc,
        &include_dir(),
        "#include <opentelemetry_c/common.h>\n#include <opentelemetry_c/trace.h>\nint main(void){return 0;}\n",
    );
    syntax_check(
        &cc,
        &include_dir(),
        "#include <opentelemetry_c/context.h>\nint main(void){otel_context_scope_t s=OTEL_CONTEXT_SCOPE_INIT; return (int)s.generation;}\n",
    );
}

#[test]
fn api_convenience_helpers_compile() {
    let cc = match find_cc() {
        Some(cc) => cc,
        None => {
            eprintln!("skipping: no C compiler found");
            return;
        }
    };
    // Exercise every optional header-only helper: the typed key/value constructors
    // (common.h) and the span-status shorthands (trace.h), including building an attribute
    // array for otel_span_add_event(). `-fsyntax-only` does not link, so a NULL span is fine.
    syntax_check(
        &cc,
        &include_dir(),
        r#"#include <opentelemetry_c/api.h>
int main(void) {
    otel_key_value_t attrs[] = {
        otel_kv_string(otel_cstr("str"), otel_cstr("v")),
        otel_kv_bool(otel_cstr("flag"), OTEL_TRUE),
        otel_kv_int64(otel_cstr("count"), 42),
        otel_kv_double(otel_cstr("ratio"), 1.5)
    };
    otel_span_t* span = (void*)0;
    otel_span_context_t* context = NULL;
    (void)otel_span_add_event(span, otel_cstr("event"), attrs, sizeof(attrs) / sizeof(attrs[0]));
    (void)otel_span_set_attribute(span, otel_kv_int64(otel_cstr("x"), 1));
    (void)otel_span_set_ok(span);
    (void)otel_span_set_error(span, otel_cstr("boom"));
    (void)otel_span_get_context(span, &context);
    (void)otel_tracer_start_span_with_context(NULL, otel_cstr("child"), NULL, context);
    {
        uint8_t trace_id[16] = {0};
        uint8_t span_id[8] = {0};
        uint8_t flags = 0;
        otel_span_context_t* built = NULL;
        trace_id[15] = 1;
        span_id[7] = 1;
        built = otel_span_context_create(trace_id, span_id, 0x01, OTEL_TRUE,
                                         otel_cstr("k=v"));
        (void)otel_span_context_is_valid(built);
        (void)otel_span_context_is_remote(built);
        (void)otel_span_context_trace_id(built, trace_id);
        (void)otel_span_context_span_id(built, span_id);
        (void)otel_span_context_trace_flags(built, &flags);
        (void)otel_span_context_tracestate(built);
        {
            char tp[64];
            size_t need = 0;
            otel_span_context_t* extracted = NULL;
            (void)otel_trace_propagation_inject_traceparent(built, tp, sizeof(tp), &need);
            (void)otel_trace_propagation_inject_tracestate(built, tp, sizeof(tp), &need);
            (void)otel_trace_propagation_extract(
                otel_cstr("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
                otel_string_view_empty(), &extracted);
            otel_span_context_destroy(extracted);
        }
        {
            otel_span_start_options_ex_t opts = OTEL_SPAN_START_OPTIONS_EX_INIT;
            otel_span_link_t links[1];
            otel_key_value_t link_attrs[] = {
                otel_kv_string(otel_cstr("k"), otel_cstr("v"))
            };
            links[0].context = built;
            links[0].attributes = link_attrs;
            links[0].attribute_count = sizeof(link_attrs) / sizeof(link_attrs[0]);
            opts.kind = OTEL_SPAN_KIND_CLIENT;
            opts.parent_context = built;
            opts.start_time_unix_nanos = 1700000000000000000ull;
            opts.attributes = link_attrs;
            opts.attribute_count = sizeof(link_attrs) / sizeof(link_attrs[0]);
            opts.links = links;
            opts.link_count = sizeof(links) / sizeof(links[0]);
            (void)otel_tracer_start_span_ex(NULL, otel_cstr("ex"), &opts);
        }
        otel_span_context_destroy(built);
    }
    otel_span_context_destroy(otel_span_context_clone(context));
    otel_span_context_destroy(context);
    return 0;
}
"#,
    );
}

#[test]
fn logs_header_compiles_and_helpers_build_structured_records() {
    let cc = match find_cc() {
        Some(cc) => cc,
        None => return,
    };
    syntax_check(
        &cc,
        &include_dir(),
        r#"#include <opentelemetry_c/logs.h>
int main(void) {
    otel_logger_provider_t* provider = otel_global_logger_provider();
    otel_logger_options_t scope = OTEL_LOGGER_OPTIONS_INIT;
    otel_logger_t* logger = NULL;
    otel_span_context_t* context = NULL;
    otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;
    /* body = ["one", {"k": 2}] laid out in the flat node pool. */
    static const uint8_t raw[] = {0xDE, 0xAD};
    otel_log_key_value_t nodes[3];
    otel_log_key_value_t attrs[2];
    nodes[0] = otel_log_element(otel_log_value_string(otel_cstr("one")));
    nodes[1] = otel_log_element(otel_log_value_map(2, 1));
    nodes[2] = otel_log_kv(otel_cstr("k"), otel_log_value_int64(2));
    attrs[0] = otel_log_kv(otel_cstr("bytes"), otel_log_value_bytes(raw, sizeof(raw)));
    attrs[1] = otel_log_kv(otel_cstr("ok"), otel_log_value_bool(OTEL_TRUE));

    scope.name = otel_cstr("scope");
    logger = otel_logger_provider_get_logger_with_options(provider, &scope);

    record.present_fields = OTEL_LOG_FIELD_TIMESTAMP | OTEL_LOG_FIELD_OBSERVED_TIMESTAMP |
                            OTEL_LOG_FIELD_TRACE_CONTEXT;
    record.timestamp_unix_nanos = 1;
    record.observed_timestamp_unix_nanos = 2;
    record.severity_number = OTEL_LOG_SEVERITY_ERROR;
    record.body = otel_log_value_array(0, 2);
    record.attributes = attrs;
    record.attribute_count = sizeof(attrs) / sizeof(attrs[0]);
    record.value_nodes = nodes;
    record.value_node_count = sizeof(nodes) / sizeof(nodes[0]);
    record.trace_context.trace_id[15] = 1;
    record.trace_context.span_id[7] = 1;
    record.trace_context.trace_flags = OTEL_LOG_TRACE_FLAGS_SAMPLED;

    if (otel_logger_enabled(logger, OTEL_LOG_SEVERITY_ERROR)) {
        (void)otel_logger_emit(logger, &record);
    }
    record.present_fields &= ~OTEL_LOG_FIELD_TRACE_CONTEXT;
    (void)otel_logger_emit_with_context(logger, &record, context);
    (void)otel_log_value_double(1.5);
    (void)otel_log_value_empty();
    otel_logger_destroy(logger);
    otel_logger_provider_destroy(provider);
    return 0;
}
"#,
    );
}

#[test]
fn metrics_header_complete_family_compiles() {
    let cc = match find_cc() {
        Some(cc) => cc,
        None => return,
    };
    syntax_check(
        &cc,
        &include_dir(),
        r#"#include <opentelemetry_c/metrics.h>
static void observe(otel_observer_u64_t* observer, void* data) {
    (void)data;
    (void)otel_observer_u64_observe(observer, 1, NULL, 0);
}
int main(void) {
    otel_meter_provider_t* provider = otel_global_meter_provider();
    otel_meter_t* meter = otel_meter_provider_get_meter(
        provider, otel_cstr("scope"), otel_string_view_empty(), otel_string_view_empty());
    otel_instrument_options_t options = OTEL_INSTRUMENT_OPTIONS_INIT;
    otel_counter_u64_t* counter = NULL;
    otel_bound_counter_u64_t* bound_counter = NULL;
    otel_observable_gauge_u64_t* observable = NULL;
    (void)otel_meter_create_u64_counter(meter, otel_cstr("counter"), &options, &counter);
    (void)otel_counter_u64_bind(counter, NULL, 0, &bound_counter);
    (void)otel_bound_counter_u64_add(bound_counter, 1);
    (void)otel_meter_create_u64_observable_gauge(
        meter, otel_cstr("observable"), &options, observe, NULL, NULL, &observable);
    otel_observable_gauge_u64_destroy(observable);
    otel_bound_counter_u64_destroy(bound_counter);
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    return 0;
}
"#,
    );
}
