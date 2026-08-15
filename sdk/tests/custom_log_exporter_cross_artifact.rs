// SPDX-License-Identifier: Apache-2.0

//! Cross-artifact custom Logs exporter proof.

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
fn logs_target_dir_resolution_honors_absolute_and_workspace_relative_values() {
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
#include <opentelemetry_c/logs.h>
#include <opentelemetry_c/sdk.h>
#include <opentelemetry_c/log_processor.h>
#include <opentelemetry_c/custom_log_exporter.h>

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct state_t {
    size_t exports;
    size_t records;
    size_t shutdowns;
    size_t destroys;
    int saw_body;
    int saw_nested;
    int saw_scope;
    int saw_resource;
    int saw_trace;
    int escaped_pointer_seen;
    int failure;
} state_t;

static int view_equals(otel_string_view_t view, const char* expected) {
    size_t len = strlen(expected);
    return view.len == len && (len == 0 || memcmp(view.ptr, expected, len) == 0);
}

static otel_status_t export_logs(void* user_data, const otel_log_export_batch_view_t* batch) {
    state_t* state = (state_t*)user_data;
    state->exports++;
    if (batch == NULL || batch->struct_size != sizeof(otel_log_export_batch_view_t)) {
        state->failure = 1;
        return OTEL_STATUS_EXPORT_FAILED;
    }
    if (batch->record_count > OTEL_LOG_EXPORT_MAX_RECORDS) {
        state->failure = 2;
        return OTEL_STATUS_EXPORT_FAILED;
    }
    for (size_t i = 0; i < batch->resource_attribute_count; i++) {
        if (view_equals(batch->resource_attributes[i].key, "service.name")) {
            state->saw_resource = 1;
        }
    }
    for (size_t i = 0; i < batch->record_count; i++) {
        const otel_log_export_record_view_t* record = &batch->records[i];
        state->records++;
        if (record->struct_size != sizeof(otel_log_export_record_view_t)) {
            state->failure = 3;
            return OTEL_STATUS_EXPORT_FAILED;
        }
        if ((record->present_fields & ~OTEL_LOG_EXPORT_FIELD_KNOWN_MASK) != 0) {
            state->failure = 4;
            return OTEL_STATUS_EXPORT_FAILED;
        }
        if (record->scope == NULL || !view_equals(record->scope->name, "cross-artifact")) {
            state->failure = 5;
            return OTEL_STATUS_EXPORT_FAILED;
        }
        state->saw_scope = 1;
        if ((record->present_fields & OTEL_LOG_EXPORT_FIELD_TRACE_CONTEXT) != 0 &&
            record->trace_context.trace_id[0] == 0x11 &&
            record->trace_context.span_id[0] == 0x22 &&
            record->trace_context.trace_flags == OTEL_LOG_TRACE_FLAGS_SAMPLED) {
            state->saw_trace = 1;
        }
        if ((record->present_fields & OTEL_LOG_EXPORT_FIELD_BODY) != 0 &&
            record->body.value_type == OTEL_LOG_VALUE_TYPE_STRING &&
            view_equals(record->body.value.string_value, "hello")) {
            state->saw_body = 1;
        }
        /* Walk the flattened attribute pool exactly as the emit path documents it. */
        for (size_t a = 0; a < record->attribute_count; a++) {
            const otel_log_key_value_t* attribute = &record->attributes[a];
            if (!view_equals(attribute->key, "nested")) {
                continue;
            }
            if (attribute->value.value_type != OTEL_LOG_VALUE_TYPE_MAP) {
                state->failure = 6;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            otel_log_value_range_t entries = attribute->value.value.children;
            if (entries.count != 1 ||
                (size_t)(entries.first + entries.count) > record->value_node_count) {
                state->failure = 7;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            const otel_log_key_value_t* entry = &record->value_nodes[entries.first];
            if (!view_equals(entry->key, "list") ||
                entry->value.value_type != OTEL_LOG_VALUE_TYPE_ARRAY) {
                state->failure = 8;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            otel_log_value_range_t elements = entry->value.value.children;
            if (elements.count != 2 || elements.first <= entries.first) {
                state->failure = 9;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            if (record->value_nodes[elements.first].value.value.int64_value != 10 ||
                record->value_nodes[elements.first + 1].value.value.int64_value != 20) {
                state->failure = 10;
                return OTEL_STATUS_EXPORT_FAILED;
            }
            state->saw_nested = 1;
        }
    }
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

static otel_status_t emit_record(otel_logger_t* logger, int with_nested) {
    otel_log_key_value_t nodes[3];
    otel_log_key_value_t attributes[1];
    otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;

    memset(nodes, 0, sizeof(nodes));
    memset(attributes, 0, sizeof(attributes));

    record.present_fields = OTEL_LOG_FIELD_TIMESTAMP | OTEL_LOG_FIELD_TRACE_CONTEXT;
    record.timestamp_unix_nanos = 1700000000000000000ull;
    record.severity_number = OTEL_LOG_SEVERITY_INFO;
    record.body.value_type = OTEL_LOG_VALUE_TYPE_STRING;
    record.body.value.string_value = otel_cstr("hello");
    memset(record.trace_context.trace_id, 0x11, sizeof(record.trace_context.trace_id));
    memset(record.trace_context.span_id, 0x22, sizeof(record.trace_context.span_id));
    record.trace_context.trace_flags = OTEL_LOG_TRACE_FLAGS_SAMPLED;

    if (with_nested) {
        /* nested = { "list": [10, 20] }, flattened per the node-pool rules. */
        nodes[0].key = otel_cstr("list");
        nodes[0].value.value_type = OTEL_LOG_VALUE_TYPE_ARRAY;
        nodes[0].value.value.children.first = 1;
        nodes[0].value.value.children.count = 2;
        nodes[1].value.value_type = OTEL_LOG_VALUE_TYPE_INT64;
        nodes[1].value.value.int64_value = 10;
        nodes[2].value.value_type = OTEL_LOG_VALUE_TYPE_INT64;
        nodes[2].value.value.int64_value = 20;

        attributes[0].key = otel_cstr("nested");
        attributes[0].value.value_type = OTEL_LOG_VALUE_TYPE_MAP;
        attributes[0].value.value.children.first = 0;
        attributes[0].value.value.children.count = 1;

        record.attributes = attributes;
        record.attribute_count = 1;
        record.value_nodes = nodes;
        record.value_node_count = 3;
    }
    return otel_logger_emit(logger, &record);
}

/* Drive one pipeline; `batched` selects the batch processor over the simple one. */
static int run_pipeline(int batched, state_t* state) {
    otel_custom_log_exporter_callbacks_t callbacks;
    otel_log_exporter_t* exporter = NULL;
    otel_log_processor_t* processor = NULL;
    otel_sdk_builder_t* builder = NULL;
    otel_sdk_t* sdk = NULL;
    otel_logger_provider_t* provider = NULL;
    otel_logger_t* logger = NULL;
    otel_logger_options_t options = OTEL_LOGGER_OPTIONS_INIT;

    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_logs = export_logs;
    callbacks.shutdown = shutdown_cb;
    callbacks.state_destroy = destroy_cb;

    if (otel_custom_log_exporter_new(&callbacks, state, &exporter) != OTEL_STATUS_OK) {
        return 10;
    }
    if (batched) {
        otel_batch_log_processor_builder_t* pb = otel_batch_log_processor_builder_new();
        otel_batch_log_processor_builder_set_exporter(pb, exporter);
        otel_batch_log_processor_builder_set_max_export_batch_size(pb, 8);
        otel_batch_log_processor_builder_set_scheduled_delay_millis(pb, 50);
        if (otel_batch_log_processor_builder_build(pb, &processor) != OTEL_STATUS_OK) {
            return 11;
        }
        otel_batch_log_processor_builder_destroy(pb);
    } else if (otel_simple_log_processor_create(exporter, &processor) != OTEL_STATUS_OK) {
        return 12;
    }

    builder = otel_sdk_builder_new();
    otel_sdk_builder_set_service_name(builder, otel_cstr("custom-log-exporter"));
    if (otel_sdk_builder_add_log_processor(builder, processor) != OTEL_STATUS_OK) {
        return 13;
    }
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        return 14;
    }
    otel_sdk_builder_destroy(builder);

    provider = otel_sdk_get_logger_provider(sdk);
    options.name = otel_cstr("cross-artifact");
    options.version = otel_cstr("0.1.0");
    logger = otel_logger_provider_get_logger_with_options(provider, &options);
    if (logger == NULL) {
        return 15;
    }
    if (emit_record(logger, 1) != OTEL_STATUS_OK) {
        return 16;
    }
    if (emit_record(logger, 0) != OTEL_STATUS_OK) {
        return 17;
    }
    otel_logger_destroy(logger);
    otel_logger_provider_destroy(provider);

    if (otel_sdk_logs_force_flush(sdk) != OTEL_STATUS_OK) {
        return 18;
    }
    if (otel_sdk_logs_shutdown(sdk, 5000) != OTEL_STATUS_OK) {
        return 19;
    }
    otel_sdk_destroy(sdk);
    return 0;
}

int main(void) {
    state_t simple;
    state_t batched;
    otel_custom_log_exporter_callbacks_t callbacks;
    otel_log_exporter_t* untransferred = NULL;
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
    if (simple.exports == 0 || simple.records != 2) {
        return 40;
    }
    if (!simple.saw_body || !simple.saw_nested || !simple.saw_scope || !simple.saw_resource ||
        !simple.saw_trace) {
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
    if (batched.records != 2 || !batched.saw_nested || !batched.saw_resource) {
        return 90;
    }
    if (batched.shutdowns != 1 || batched.destroys != 1) {
        return 91;
    }

    /* An exporter that is never transferred must still shut down and release its state. */
    memset(&untransferred_state, 0, sizeof(untransferred_state));
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_logs = export_logs;
    callbacks.shutdown = shutdown_cb;
    callbacks.state_destroy = destroy_cb;
    if (otel_custom_log_exporter_new(&callbacks, &untransferred_state, &untransferred) !=
        OTEL_STATUS_OK) {
        return 92;
    }
    otel_log_exporter_destroy(untransferred);
    if (untransferred_state.shutdowns != 1 || untransferred_state.destroys != 1) {
        return 93;
    }

    /* A rejected construction must leave the callback state untouched. */
    memset(&untransferred_state, 0, sizeof(untransferred_state));
    callbacks.export_logs = NULL;
    untransferred = NULL;
    if (otel_custom_log_exporter_new(&callbacks, &untransferred_state, &untransferred) ==
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
fn custom_log_exporter_works_across_shared_libraries() {
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
        "otel_c_custom_logs_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let binary = std::env::temp_dir().join(unique);
    let source = binary.with_extension("c");
    std::fs::write(&source, HARNESS).expect("write custom Logs harness");

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
        .expect("compile custom Logs harness");
    assert!(
        compile.status.success(),
        "custom Logs harness failed to compile:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&binary)
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .expect("run custom Logs harness");
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&binary);
    assert!(
        run.status.success(),
        "custom Logs harness failed with {:?}:\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
