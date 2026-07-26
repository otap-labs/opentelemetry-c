//! Cross-artifact custom Metrics exporter and manual-reader proof.

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
fn target_dir_resolution_honors_absolute_and_workspace_relative_values() {
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

const HARNESS: &str = r#"
#include <opentelemetry_c/metrics.h>
#include <opentelemetry_c/sdk.h>

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct state_t {
    size_t exports;
    size_t flushes;
    size_t shutdowns;
    size_t destroys;
    uint64_t value;
    int saw_name;
    const otel_metric_batch_t* stale_batch;
} state_t;

static int view_equals(otel_string_view_t view, const char* expected) {
    size_t len = strlen(expected);
    return view.len == len && memcmp(view.ptr, expected, len) == 0;
}

static otel_status_t visit_metric(void* data, const otel_metric_metadata_t* metadata) {
    state_t* state = (state_t*)data;
    if (metadata->data_kind != OTEL_METRIC_DATA_SUM ||
        metadata->number_kind != OTEL_METRIC_NUMBER_U64) {
        return OTEL_STATUS_EXPORT_FAILED;
    }
    state->saw_name = view_equals(metadata->name, "cross_artifact_requests");
    return OTEL_STATUS_OK;
}

static otel_status_t visit_point(
    void* data,
    const otel_metric_point_t* point,
    const otel_metric_attribute_t* attributes,
    size_t attribute_count,
    const double* explicit_bounds,
    size_t explicit_bound_count,
    const uint64_t* explicit_bucket_counts,
    size_t explicit_bucket_count,
    const uint64_t* positive_bucket_counts,
    size_t positive_bucket_count,
    const uint64_t* negative_bucket_counts,
    size_t negative_bucket_count) {
    state_t* state = (state_t*)data;
    (void)attributes;
    (void)attribute_count;
    (void)explicit_bounds;
    (void)explicit_bound_count;
    (void)explicit_bucket_counts;
    (void)explicit_bucket_count;
    (void)positive_bucket_counts;
    (void)positive_bucket_count;
    (void)negative_bucket_counts;
    (void)negative_bucket_count;
    state->value = point->value.u64_value;
    return OTEL_STATUS_OK;
}

static otel_status_t export_metrics(void* data, const otel_metric_batch_t* batch) {
    state_t* state = (state_t*)data;
    otel_metric_visitor_t visitor;
    memset(&visitor, 0, sizeof(visitor));
    visitor.struct_size = sizeof(visitor);
    visitor.metric = visit_metric;
    visitor.point = visit_point;
    state->exports++;
    state->stale_batch = batch;
    return otel_metric_batch_visit(batch, &visitor, data);
}

static otel_status_t force_flush(void* data) {
    ((state_t*)data)->flushes++;
    return OTEL_STATUS_OK;
}

static otel_status_t shutdown_exporter(void* data, uint64_t timeout_millis) {
    (void)timeout_millis;
    ((state_t*)data)->shutdowns++;
    return OTEL_STATUS_OK;
}

static void destroy_state(void* data) {
    state_t* state = (state_t*)data;
    state->destroys++;
}

int main(void) {
    state_t state;
    memset(&state, 0, sizeof(state));

    otel_custom_metric_exporter_callbacks_t callbacks;
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.struct_size = sizeof(callbacks);
    callbacks.export_metrics = export_metrics;
    callbacks.force_flush = force_flush;
    callbacks.shutdown = shutdown_exporter;
    callbacks.state_destroy = destroy_state;

    otel_metric_exporter_t* exporter = NULL;
    if (otel_custom_metric_exporter_new(
            &callbacks, &state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &exporter) !=
        OTEL_STATUS_OK) {
        return 10;
    }
    otel_manual_metric_reader_t* reader = NULL;
    if (otel_manual_metric_reader_new(exporter, &reader) != OTEL_STATUS_OK) {
        return 11;
    }
    otel_sdk_builder_t* builder = otel_sdk_builder_new();
    if (builder == NULL ||
        otel_sdk_builder_add_manual_metric_reader(builder, reader) != OTEL_STATUS_OK) {
        return 12;
    }
    otel_sdk_t* sdk = NULL;
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        return 13;
    }
    otel_sdk_builder_destroy(builder);
    if (otel_sdk_set_metrics_as_global(sdk) != OTEL_STATUS_OK) {
        return 14;
    }

    otel_meter_provider_t* provider = otel_global_meter_provider();
    otel_meter_t* meter = otel_meter_provider_get_meter(
        provider, otel_cstr("custom-cross-artifact"), otel_cstr(""), otel_cstr(""));
    otel_instrument_options_t options;
    memset(&options, 0, sizeof(options));
    options.struct_size = sizeof(options);
    otel_counter_u64_t* counter = NULL;
    if (meter == NULL ||
        otel_meter_create_u64_counter(
            meter, otel_cstr("cross_artifact_requests"), &options, &counter) !=
            OTEL_STATUS_OK ||
        otel_counter_u64_add(counter, 13, NULL, 0) != OTEL_STATUS_OK) {
        return 15;
    }
    if (otel_sdk_metrics_force_flush(sdk, 0) != OTEL_STATUS_OK) {
        return 16;
    }
    if (state.exports != 1 || state.flushes != 1 || !state.saw_name || state.value != 13) {
        return 17;
    }

    otel_metric_visitor_t stale_visitor;
    memset(&stale_visitor, 0, sizeof(stale_visitor));
    stale_visitor.struct_size = sizeof(stale_visitor);
    if (otel_metric_batch_visit(state.stale_batch, &stale_visitor, NULL) !=
        OTEL_STATUS_INVALID_ARGUMENT) {
        return 18;
    }

    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    if (otel_sdk_metrics_shutdown(sdk, 1000) != OTEL_STATUS_OK) {
        return 19;
    }
    otel_sdk_destroy(sdk);
    if (state.shutdowns != 1 || state.destroys != 1) {
        return 20;
    }

#ifdef OTEL_TEST_ASYNC_READER
    state_t async_state;
    memset(&async_state, 0, sizeof(async_state));
    otel_metric_exporter_t* async_exporter = NULL;
    if (otel_custom_metric_exporter_new(
            &callbacks, &async_state, OTEL_METRIC_TEMPORALITY_CUMULATIVE, &async_exporter) !=
        OTEL_STATUS_OK) {
        return 21;
    }
    otel_periodic_metric_reader_builder_t* reader_builder =
        otel_periodic_metric_reader_builder_new();
    if (reader_builder == NULL ||
        otel_periodic_metric_reader_builder_set_runtime(
            reader_builder, OTEL_METRIC_READER_RUNTIME_ASYNC) != OTEL_STATUS_OK ||
        otel_periodic_metric_reader_builder_set_interval_millis(reader_builder, 60000) !=
            OTEL_STATUS_OK ||
        otel_periodic_metric_reader_builder_set_timeout_millis(reader_builder, 1000) !=
            OTEL_STATUS_OK ||
        otel_periodic_metric_reader_builder_set_exporter(reader_builder, async_exporter) !=
            OTEL_STATUS_OK) {
        return 22;
    }
    otel_periodic_metric_reader_t* async_reader = NULL;
    if (otel_periodic_metric_reader_builder_build(reader_builder, &async_reader) !=
        OTEL_STATUS_OK) {
        return 23;
    }
    otel_periodic_metric_reader_builder_destroy(reader_builder);
    builder = otel_sdk_builder_new();
    if (builder == NULL ||
        otel_sdk_builder_add_metric_reader(builder, async_reader) != OTEL_STATUS_OK) {
        return 24;
    }
    sdk = NULL;
    if (otel_sdk_build(builder, &sdk) != OTEL_STATUS_OK) {
        return 25;
    }
    otel_sdk_builder_destroy(builder);
    if (otel_sdk_set_metrics_as_global(sdk) != OTEL_STATUS_OK) {
        return 26;
    }
    provider = otel_global_meter_provider();
    meter = otel_meter_provider_get_meter(
        provider, otel_cstr("custom-async-cross-artifact"), otel_cstr(""), otel_cstr(""));
    counter = NULL;
    if (meter == NULL ||
        otel_meter_create_u64_counter(
            meter, otel_cstr("cross_artifact_requests"), &options, &counter) !=
            OTEL_STATUS_OK ||
        otel_counter_u64_add(counter, 29, NULL, 0) != OTEL_STATUS_OK) {
        return 27;
    }
    size_t exports_before_flush = async_state.exports;
    if (otel_sdk_metrics_force_flush(sdk, 0) != OTEL_STATUS_OK ||
        async_state.exports <= exports_before_flush ||
        !async_state.saw_name || async_state.value != 29) {
        return 28;
    }
    otel_counter_u64_destroy(counter);
    otel_meter_destroy(meter);
    otel_meter_provider_destroy(provider);
    if (otel_sdk_metrics_shutdown(sdk, 1000) != OTEL_STATUS_OK) {
        return 29;
    }
    otel_sdk_destroy(sdk);
    if (async_state.shutdowns != 1 || async_state.destroys != 1) {
        return 30;
    }
#endif

    return 0;
}
"#;

#[test]
fn custom_exporter_and_manual_reader_work_across_shared_libraries() {
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
        "otel_c_custom_metrics_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let binary = std::env::temp_dir().join(unique);
    let source = binary.with_extension("c");
    std::fs::write(&source, HARNESS).expect("write custom Metrics harness");

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
    #[cfg(feature = "metrics-async-runtime")]
    compile_command.arg("-DOTEL_TEST_ASYNC_READER=1");
    let compile = compile_command
        .output()
        .expect("compile custom Metrics harness");
    assert!(
        compile.status.success(),
        "custom Metrics harness failed to compile:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&binary)
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .expect("run custom Metrics harness");
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&binary);
    assert!(
        run.status.success(),
        "custom Metrics harness failed with {:?}:\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
