// SPDX-License-Identifier: Apache-2.0

//! Cross-artifact custom Traces exporter proof.
//!
//! Compiles a standalone C program that includes only the public headers, links both
//! `libopentelemetry_c_api` and `libopentelemetry_c_sdk`, registers a callback-backed span
//! exporter through a simple and a batch span processor, and asserts that the exported span
//! batch view carries the emitted resource, scope, span, attribute, and event data with the
//! documented callback-state lifecycle (shutdown once, then state_destroy once).

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    ["cc", "clang", "gcc"].into_iter().find_map(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| candidate.to_owned())
    })
}

fn resolve_target_dir(workspace_root: &Path, configured: Option<PathBuf>) -> PathBuf {
    match configured {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => workspace_root.join(dir),
        None => workspace_root.join("target"),
    }
}

fn profile_dirs(target_dir: &Path, configured_target: Option<PathBuf>) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(target) = configured_target {
        directories.push(target_dir.join(&target).join("debug"));
        directories.push(target_dir.join(target).join("release"));
    }
    directories.push(target_dir.join("debug"));
    directories.push(target_dir.join("release"));
    directories
}

fn find_lib_dir() -> Option<PathBuf> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let configured = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let target_dir = resolve_target_dir(workspace_root, configured);
    let configured_target = std::env::var_os("CARGO_BUILD_TARGET")
        .filter(|target| !target.is_empty())
        .map(PathBuf::from);
    for directory in profile_dirs(&target_dir, configured_target) {
        let api = ["libopentelemetry_c_api.dylib", "libopentelemetry_c_api.so"]
            .into_iter()
            .any(|name| directory.join(name).is_file());
        let sdk = ["libopentelemetry_c_sdk.dylib", "libopentelemetry_c_sdk.so"]
            .into_iter()
            .any(|name| directory.join(name).is_file());
        if api && sdk {
            return Some(directory);
        }
    }
    None
}

#[test]
fn traces_target_dir_resolution_honors_absolute_and_workspace_relative_values() {
    let workspace_root = std::env::temp_dir().join("otel-c-workspace");
    let absolute = std::env::temp_dir().join("otel-c-target");
    assert_eq!(
        resolve_target_dir(&workspace_root, Some(absolute.clone())),
        absolute
    );
    assert_eq!(
        resolve_target_dir(&workspace_root, Some(PathBuf::from("build/target"))),
        workspace_root.join("build/target")
    );
    assert_eq!(
        resolve_target_dir(&workspace_root, None),
        workspace_root.join("target")
    );
    assert_eq!(
        profile_dirs(Path::new("/tmp/target"), None),
        [
            PathBuf::from("/tmp/target/debug"),
            PathBuf::from("/tmp/target/release")
        ]
    );
    assert_eq!(
        profile_dirs(
            Path::new("/tmp/target"),
            Some(PathBuf::from("x86_64-unknown-linux-gnu"))
        ),
        [
            PathBuf::from("/tmp/target/x86_64-unknown-linux-gnu/debug"),
            PathBuf::from("/tmp/target/x86_64-unknown-linux-gnu/release"),
            PathBuf::from("/tmp/target/debug"),
            PathBuf::from("/tmp/target/release"),
        ]
    );
}

fn is_ci() -> bool {
    std::env::var("CI")
        .map(|value| !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

const HARNESS: &str = r##"
#include <opentelemetry_c/api.h>
#include <opentelemetry_c/sdk.h>
#include <opentelemetry_c/custom_trace_exporter.h>
#include <opentelemetry_c/simple_span_processor.h>
#include <opentelemetry_c/batch_span_processor.h>

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct state_t {
    size_t exports;
    size_t spans;
    size_t shutdowns;
    size_t destroys;
    int saw_scope;
    int saw_resource;
    int saw_kind;
    int saw_status;
    int saw_attr;
    int saw_event;
    int failure;
} state_t;

static int view_equals(otel_string_view_t view, const char* expected) {
    size_t len = strlen(expected);
    return view.len == len && (len == 0 || memcmp(view.ptr, expected, len) == 0);
}

static otel_status_t export_spans(void* user_data, const otel_span_export_batch_view_t* batch) {
    state_t* state = (state_t*)user_data;
    state->exports++;
    if (batch == NULL || batch->struct_size != sizeof(otel_span_export_batch_view_t)) {
        state->failure = 1;
        return OTEL_STATUS_EXPORT_FAILED;
    }
    if (batch->record_count > OTEL_SPAN_EXPORT_MAX_SPANS) {
        state->failure = 2;
        return OTEL_STATUS_EXPORT_FAILED;
    }
    for (size_t i = 0; i < batch->resource_attribute_count; i++) {
        if (view_equals(batch->resource_attributes[i].key, "service.name")) {
            state->saw_resource = 1;
        }
    }
    for (size_t i = 0; i < batch->record_count; i++) {
        const otel_span_export_record_view_t* record = &batch->records[i];
        state->spans++;
        if (record->struct_size != sizeof(otel_span_export_record_view_t)) {
            state->failure = 3;
            return OTEL_STATUS_EXPORT_FAILED;
        }
        if (record->scope == NULL || !view_equals(record->scope->name, "cross-artifact")) {
            state->failure = 4;
            return OTEL_STATUS_EXPORT_FAILED;
        }
        state->saw_scope = 1;
        if (!view_equals(record->name, "handle-request")) {
            continue;
        }
        if (record->span_kind == OTEL_SPAN_KIND_SERVER) {
            state->saw_kind = 1;
        }
        if (record->status_code == OTEL_SPAN_STATUS_OK) {
            state->saw_status = 1;
        }
        for (size_t a = 0; a < record->attribute_count; a++) {
            const otel_span_attribute_t* attribute = &record->attributes[a];
            if (attribute->value_type > OTEL_SPAN_ATTRIBUTE_TYPE_DOUBLE_ARRAY) {
                state->failure = 5;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            if (view_equals(attribute->key, "http.request.method") &&
                attribute->value_type == OTEL_ATTRIBUTE_TYPE_STRING &&
                view_equals(attribute->value.scalar.string_value, "GET")) {
                state->saw_attr = 1;
            }
        }
        for (size_t e = 0; e < record->event_count; e++) {
            const otel_span_event_view_t* event = &record->events[e];
            if (event->struct_size != sizeof(otel_span_event_view_t)) {
                state->failure = 6;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            if (view_equals(event->name, "lookup")) {
                state->saw_event = 1;
            }
        }
    }
    return OTEL_STATUS_OK;
}

static otel_status_t force_flush_cb(void* user_data) {
    (void)user_data;
    return OTEL_STATUS_OK;
}

static otel_status_t shutdown_cb(void* user_data, uint64_t timeout_millis) {
    state_t* state = (state_t*)user_data;
    (void)timeout_millis;
    state->shutdowns++;
    return OTEL_STATUS_OK;
}

static void destroy_cb(void* user_data) {
    state_t* state = (state_t*)user_data;
    state->destroys++;
}

static otel_status_t emit_span(otel_tracer_t* tracer, int with_extras) {
    otel_span_start_options_t opts;
    otel_span_t* span = NULL;
    otel_status_t status;

    opts.parent = NULL;
    if (with_extras) {
        opts.kind = OTEL_SPAN_KIND_SERVER;
        span = otel_tracer_start_span(tracer, otel_cstr("handle-request"), &opts);
        if (span == NULL) {
            return OTEL_STATUS_INVALID_ARGUMENT;
        }
        otel_span_set_string_attribute(span, otel_cstr("http.request.method"), otel_cstr("GET"));
        otel_span_set_int64_attribute(span, otel_cstr("http.response.status_code"), 200);
        otel_key_value_t event_attr = otel_kv_string(otel_cstr("cache"), otel_cstr("miss"));
        otel_span_add_event(span, otel_cstr("lookup"), &event_attr, 1);
    } else {
        opts.kind = OTEL_SPAN_KIND_INTERNAL;
        span = otel_tracer_start_span(tracer, otel_cstr("background"), &opts);
        if (span == NULL) {
            return OTEL_STATUS_INVALID_ARGUMENT;
        }
    }
    otel_span_set_ok(span);
    status = otel_span_end(span);
    otel_span_destroy(span);
    return status;
}

/* Drive one pipeline; `batched` selects the batch processor over the simple one. */
static int run_pipeline(int batched, state_t* state) {
    otel_custom_trace_exporter_callbacks_t callbacks;
    otel_trace_exporter_t* exporter = NULL;
    otel_span_processor_t* processor = NULL;
    otel_sdk_builder_t* builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_tracer_provider_t* provider = NULL;
    otel_tracer_t* tracer = NULL;

    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_spans = export_spans;
    callbacks.force_flush = force_flush_cb;
    callbacks.shutdown = shutdown_cb;
    callbacks.state_destroy = destroy_cb;

    if (otel_custom_trace_exporter_new(&callbacks, state, &exporter) != OTEL_STATUS_OK) {
        return 10;
    }
    if (batched) {
        otel_batch_span_processor_builder_t* pb = otel_batch_span_processor_builder_new();
        otel_batch_span_processor_builder_set_exporter(pb, exporter);
        otel_batch_span_processor_builder_set_max_export_batch_size(pb, 8);
        otel_batch_span_processor_builder_set_scheduled_delay_millis(pb, 50);
        if (otel_batch_span_processor_builder_build(pb, &processor) != OTEL_STATUS_OK) {
            return 11;
        }
        otel_batch_span_processor_builder_destroy(pb);
    } else if (otel_simple_span_processor_create(exporter, &processor) != OTEL_STATUS_OK) {
        return 12;
    }

    builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(builder, otel_cstr("custom-trace-exporter"));
    if (otel_sdk_builder_add_span_processor(builder, processor) != OTEL_STATUS_OK) {
        return 13;
    }
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        return 14;
    }
    otel_sdk_builder_destroy(builder);

    provider = otel_sdk_get_tracer_provider(sdk);
    tracer = otel_tracer_provider_get_tracer(
        provider, otel_cstr("cross-artifact"), otel_cstr("0.1.0"), otel_string_view_empty());
    if (tracer == NULL) {
        return 15;
    }
    if (emit_span(tracer, 1) != OTEL_STATUS_OK) {
        return 16;
    }
    if (emit_span(tracer, 0) != OTEL_STATUS_OK) {
        return 17;
    }
    otel_tracer_destroy(tracer);
    otel_tracer_provider_destroy(provider);

    if (otel_sdk_force_flush(sdk, 5000) != OTEL_STATUS_OK) {
        return 18;
    }
    if (otel_sdk_shutdown(sdk, 5000) != OTEL_STATUS_OK) {
        return 19;
    }
    otel_sdk_destroy(sdk);
    return 0;
}

int main(void) {
    state_t simple;
    state_t batched;
    otel_custom_trace_exporter_callbacks_t callbacks;
    otel_trace_exporter_t* untransferred = NULL;
    state_t untransferred_state;
    int rc;

    memset(&simple, 0, sizeof(simple));
    rc = run_pipeline(0, &simple);
    if (rc != 0) {
        return rc;
    }
    if (simple.failure != 0) {
        return 20 + simple.failure;
    }
    if (simple.exports == 0 || simple.spans != 2) {
        return 40;
    }
    if (!simple.saw_scope || !simple.saw_resource || !simple.saw_kind || !simple.saw_status ||
        !simple.saw_attr || !simple.saw_event) {
        return 41;
    }
    if (simple.shutdowns != 1 || simple.destroys != 1) {
        return 42;
    }

    memset(&batched, 0, sizeof(batched));
    rc = run_pipeline(1, &batched);
    if (rc != 0) {
        return 50 + rc;
    }
    if (batched.failure != 0) {
        return 70 + batched.failure;
    }
    if (batched.spans != 2 || !batched.saw_attr || !batched.saw_event || !batched.saw_resource) {
        return 90;
    }
    if (batched.shutdowns != 1 || batched.destroys != 1) {
        return 91;
    }

    /* An exporter that is never transferred must still shut down and release its state. */
    memset(&untransferred_state, 0, sizeof(untransferred_state));
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_spans = export_spans;
    callbacks.force_flush = force_flush_cb;
    callbacks.shutdown = shutdown_cb;
    callbacks.state_destroy = destroy_cb;
    if (otel_custom_trace_exporter_new(&callbacks, &untransferred_state, &untransferred) !=
        OTEL_STATUS_OK) {
        return 92;
    }
    otel_trace_exporter_destroy(untransferred);
    if (untransferred_state.shutdowns != 1 || untransferred_state.destroys != 1) {
        return 93;
    }

    /* A rejected construction must leave the callback state untouched. */
    memset(&untransferred_state, 0, sizeof(untransferred_state));
    callbacks.export_spans = NULL;
    untransferred = NULL;
    if (otel_custom_trace_exporter_new(&callbacks, &untransferred_state, &untransferred) ==
        OTEL_STATUS_OK) {
        return 94;
    }
    if (untransferred != NULL || untransferred_state.destroys != 0) {
        return 95;
    }

    return 0;
}
"##;

#[test]
fn custom_trace_exporter_works_across_shared_libraries() {
    let Some(cc) = find_cc() else {
        if is_ci() {
            panic!("CI=true but no C compiler is available");
        }
        eprintln!("skipping: no C compiler");
        return;
    };
    let Some(lib_dir) = find_lib_dir() else {
        if is_ci() {
            panic!(
                "CI=true but API/SDK cdylibs are not built; build both workspace libraries first"
            );
        }
        eprintln!("skipping: API/SDK cdylibs are not built");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let unique = format!(
        "otel_c_custom_traces_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let binary = std::env::temp_dir().join(unique);
    let source = binary.with_extension("c");
    std::fs::write(&source, HARNESS).expect("write custom Traces harness");

    let mut compile_command = Command::new(cc);
    compile_command
        .arg("-std=c11")
        .arg(&source)
        .arg("-I")
        .arg(root.join("api/include"))
        .arg("-I")
        .arg(root.join("sdk/include"))
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lopentelemetry_c_api")
        .arg("-lopentelemetry_c_sdk")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&binary);
    if let Ok(flags) = std::env::var("CFLAGS") {
        compile_command.args(flags.split_whitespace());
    }
    let compile = compile_command
        .output()
        .expect("compile custom Traces harness");
    assert!(
        compile.status.success(),
        "custom Traces harness failed to compile:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&binary)
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .expect("run custom Traces harness");
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&binary);
    assert!(
        run.status.success(),
        "custom Traces harness failed with {:?}:\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
