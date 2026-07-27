//! Cross-artifact proof for the experimental **Logs** bridge.
//!
//! Compiles a C program, links it against BOTH `libopentelemetry_c_api` and
//! `libopentelemetry_c_sdk`, and runs it. The program builds a Logs pipeline through the SDK,
//! installs it, and then emits records using ONLY the API's global-logger path — exactly as an
//! instrumentation library that links the API alone would. A self-contained mock OTLP/HTTP
//! collector captures the export, which is decoded with `opentelemetry-proto` to assert the
//! wire shape.
//!
//! Beyond "bytes arrived", this test pins the documented behavior of the bridge:
//!   * the record body and every `AnyValue` kind (string/bool/int/double/bytes/array/map)
//!     survive the flat node-pool conversion with the right nesting;
//!   * `severity_text` is synthesized from the severity number (the pinned Rust setter takes
//!     `&'static str`, so it cannot come from the caller);
//!   * `event_name` is **absent** on the wire, which is the deliberate phase-1 limitation;
//!   * explicit trace context reaches the exported record, and `observed_time_unix_nano` is
//!     filled in by the SDK even though the caller left it unset;
//!   * duplicate top-level attribute keys are preserved (the pinned record appends).
//!
//! Self-skips when a C compiler or the cdylibs are unavailable, but **fails hard** under `CI`
//! so the proof can never silently no-op there.

#![cfg(feature = "otlp-http")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use prost::Message;

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
            .map(|output| output.status.success())
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

fn is_ci() -> bool {
    std::env::var("CI")
        .map(|value| !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Find a target profile dir containing BOTH cdylibs. Mirrors `cross_artifact.rs`.
fn find_lib_dir() -> Option<PathBuf> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
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
    let mut directories = Vec::new();
    if let Some(target) = std::env::var_os("CARGO_BUILD_TARGET").filter(|target| !target.is_empty())
    {
        let target = PathBuf::from(target);
        directories.push(target_dir.join(&target).join("debug"));
        directories.push(target_dir.join(target).join("release"));
    }
    directories.push(target_dir.join("debug"));
    directories.push(target_dir.join("release"));
    for dir in directories {
        let has = |stem: &str| dylib_names(stem).iter().any(|n| dir.join(n).exists());
        if has("opentelemetry_c_api") && has("opentelemetry_c_sdk") {
            return Some(dir);
        }
    }
    None
}

/// The C harness. Note that after `otel_sdk_set_logs_as_global()` it never touches the SDK
/// handle again for emission: every record goes through `otel_global_logger_provider()`,
/// which lives in the *API* library. That is the whole point of the proof.
const HARNESS_C: &str = r##"
#include <opentelemetry_c/logs.h>
#include <opentelemetry_c/sdk.h>
#include <opentelemetry_c/log_exporter.h>
#include <opentelemetry_c/log_processor.h>
#include <opentelemetry_c/otlp_log_exporter.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static otel_string_view_t cs(const char* s) {
    otel_string_view_t v;
    v.ptr = s;
    v.len = s ? strlen(s) : 0;
    return v;
}

#define CHECK(expr)                                                        \
    do {                                                                   \
        otel_status_t _st = (expr);                                        \
        if (_st != OTEL_STATUS_OK) {                                       \
            otel_string_view_t _msg = otel_last_error_message();           \
            fprintf(stderr, "%s failed: %d (%.*s)\n", #expr, (int)_st,     \
                    (int)_msg.len, _msg.ptr ? (const char*)_msg.ptr : ""); \
            return 1;                                                      \
        }                                                                  \
    } while (0)

int main(void) {
    const char* endpoint = getenv("OTEL_TEST_LOGS_ENDPOINT");
    if (!endpoint) {
        fprintf(stderr, "OTEL_TEST_LOGS_ENDPOINT is required\n");
        return 1;
    }

    otel_otlp_log_exporter_builder_t* eb = otel_otlp_log_exporter_builder_new();
    if (!eb) { return 1; }
    CHECK(otel_otlp_log_exporter_builder_set_endpoint(eb, cs(endpoint)));
    CHECK(otel_otlp_log_exporter_builder_set_transport(
        eb, OTEL_OTLP_LOG_TRANSPORT_HTTP_PROTOBUF));
    otel_log_exporter_t* exporter = NULL;
    CHECK(otel_otlp_log_exporter_builder_build(eb, &exporter));
    otel_otlp_log_exporter_builder_destroy(eb);

    /* A simple processor keeps the test deterministic: the export happens inside emit. */
    otel_log_processor_t* processor = NULL;
    CHECK(otel_simple_log_processor_create(exporter, &processor));

    otel_sdk_builder_t* sb = otel_sdk_builder_new();
    if (!sb) { return 1; }
    CHECK(otel_sdk_builder_set_service_name(sb, cs("logs-cross-artifact")));
    CHECK(otel_sdk_builder_add_log_processor(sb, processor));
    otel_sdk_t* sdk = NULL;
    CHECK(otel_sdk_build(sb, &sdk));
    otel_sdk_builder_destroy(sb);
    CHECK(otel_sdk_set_logs_as_global(sdk));

    /* ---- From here on, API-only calls. ---- */
    otel_logger_provider_t* provider = otel_global_logger_provider();
    if (!provider) {
        fprintf(stderr, "global logger provider is NULL after install\n");
        return 1;
    }
    otel_logger_options_t options = OTEL_LOGGER_OPTIONS_INIT;
    options.name = cs("cross-artifact-scope");
    options.version = cs("1.2.3");
    options.schema_url = cs("https://example.invalid/schema");
    otel_logger_t* logger = otel_logger_provider_get_logger_with_options(provider, &options);
    if (!logger) {
        otel_string_view_t msg = otel_last_error_message();
        fprintf(stderr, "logger acquisition failed: %.*s\n", (int)msg.len,
                msg.ptr ? (const char*)msg.ptr : "");
        return 1;
    }
    if (!otel_logger_enabled(logger, OTEL_LOG_SEVERITY_ERROR)) {
        fprintf(stderr, "an SDK-backed logger must be enabled for ERROR\n");
        return 1;
    }
    /* Severity 0 is "absent" and must never be reported as enabled. */
    if (otel_logger_enabled(logger, OTEL_LOG_SEVERITY_UNSPECIFIED)) {
        fprintf(stderr, "severity 0 must not be reported as enabled\n");
        return 1;
    }

    /*
     * Node pool layout. Children must live at a STRICTLY greater index than their parent:
     *
     *   [0] map entry  "inner"  -> array at [2..4)
     *   [1] map entry  "flag"   -> bool
     *   [2] array elem          -> int64 7
     *   [3] array elem          -> bytes {0xDE,0xAD}
     */
    static const uint8_t raw[] = {0xDE, 0xAD};
    otel_log_key_value_t pool[4];
    pool[0] = otel_log_kv(cs("inner"), otel_log_value_array(2, 2));
    pool[1] = otel_log_kv(cs("flag"), otel_log_value_bool(1));
    pool[2] = otel_log_element(otel_log_value_int64(7));
    pool[3] = otel_log_element(otel_log_value_bytes(raw, sizeof(raw)));

    otel_log_key_value_t attributes[4];
    attributes[0] = otel_log_kv(cs("structured"), otel_log_value_map(0, 2));
    attributes[1] = otel_log_kv(cs("ratio"), otel_log_value_double(0.5));
    /* Duplicate top-level keys are documented as preserved, not deduplicated. */
    attributes[2] = otel_log_kv(cs("dup"), otel_log_value_string(cs("first")));
    attributes[3] = otel_log_kv(cs("dup"), otel_log_value_string(cs("second")));

    otel_log_record_view_t record = OTEL_LOG_RECORD_VIEW_INIT;
    record.severity_number = OTEL_LOG_SEVERITY_ERROR;
    record.body = otel_log_value_string(cs("cross artifact log body"));
    record.attributes = attributes;
    record.attribute_count = 4;
    record.value_nodes = pool;
    record.value_node_count = 4;
    /* Set timestamp but deliberately NOT observed_timestamp: the SDK must fill that in. */
    record.present_fields = OTEL_LOG_FIELD_TIMESTAMP | OTEL_LOG_FIELD_TRACE_CONTEXT;
    record.timestamp_unix_nanos = 1700000000000000000ULL;
    for (int i = 0; i < 16; i++) { record.trace_context.trace_id[i] = (uint8_t)(i + 1); }
    for (int i = 0; i < 8; i++) { record.trace_context.span_id[i] = (uint8_t)(0x10 + i); }
    record.trace_context.trace_flags = OTEL_LOG_TRACE_FLAGS_SAMPLED;
    CHECK(otel_logger_emit(logger, &record));

    /* A record with no body at all is valid; EMPTY is only allowed in that position. */
    otel_log_record_view_t minimal = OTEL_LOG_RECORD_VIEW_INIT;
    minimal.severity_number = OTEL_LOG_SEVERITY_INFO;
    CHECK(otel_logger_emit(logger, &minimal));

    /* An out-of-range severity is rejected and emits nothing. */
    otel_log_record_view_t bad = OTEL_LOG_RECORD_VIEW_INIT;
    bad.severity_number = 25;
    if (otel_logger_emit(logger, &bad) != OTEL_STATUS_INVALID_ARGUMENT) {
        fprintf(stderr, "severity 25 must be rejected\n");
        return 1;
    }
    /* A child range that points backwards must be rejected (cycle prevention). */
    otel_log_key_value_t cyclic[2];
    cyclic[0] = otel_log_kv(cs("a"), otel_log_value_map(0, 1));
    cyclic[1] = otel_log_element(otel_log_value_int64(1));
    otel_log_record_view_t backwards = OTEL_LOG_RECORD_VIEW_INIT;
    backwards.severity_number = OTEL_LOG_SEVERITY_INFO;
    backwards.body = otel_log_value_map(0, 1);
    backwards.value_nodes = cyclic;
    backwards.value_node_count = 2;
    if (otel_logger_emit(logger, &backwards) == OTEL_STATUS_OK) {
        fprintf(stderr, "a self-referencing node pool must be rejected\n");
        return 1;
    }

    otel_logger_destroy(logger);
    otel_logger_provider_destroy(provider);
    CHECK(otel_sdk_logs_force_flush(sdk, 0));
    CHECK(otel_sdk_logs_shutdown(sdk, 5000));
    otel_sdk_destroy(sdk);
    return 0;
}
"##;

struct MockCollector {
    port: u16,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

/// Minimal mock OTLP/HTTP collector retaining complete `POST /v1/logs` bodies.
fn start_mock() -> MockCollector {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock collector");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (bodies_thread, stop_thread) = (Arc::clone(&bodies), Arc::clone(&stop));
    let thread = std::thread::spawn(move || {
        while !stop_thread.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut content_length = None;
                    let mut header_end = None;
                    loop {
                        match socket.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buffer.extend_from_slice(&chunk[..n]);
                                if header_end.is_none() {
                                    if let Some(pos) =
                                        buffer.windows(4).position(|w| w == b"\r\n\r\n")
                                    {
                                        header_end = Some(pos + 4);
                                        let headers =
                                            String::from_utf8_lossy(&buffer[..pos]).to_lowercase();
                                        for line in headers.lines() {
                                            if let Some(value) =
                                                line.strip_prefix("content-length:")
                                            {
                                                content_length = value.trim().parse().ok();
                                            }
                                        }
                                    }
                                }
                                if let (Some(end), Some(length)) = (header_end, content_length) {
                                    if buffer.len().saturating_sub(end) >= length {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    if let (Some(end), Some(length)) = (header_end, content_length) {
                        if buffer.len().saturating_sub(end) >= length
                            && String::from_utf8_lossy(&buffer[..end]).contains("POST /v1/logs")
                        {
                            bodies_thread
                                .lock()
                                .unwrap()
                                .push(buffer[end..end + length].to_vec());
                        }
                    }
                    let _ = socket.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\n\r\n",
                    );
                }
                Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    MockCollector {
        port,
        bodies,
        stop,
        thread,
    }
}

fn string_of(value: &AnyValue) -> Option<&str> {
    match value.value.as_ref()? {
        any_value::Value::StringValue(s) => Some(s.as_str()),
        _ => None,
    }
}

fn attribute<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
}

/// Assert the exported protobuf matches the documented Logs contract.
fn assert_exported_logs(bodies: &[Vec<u8>]) {
    let requests: Vec<ExportLogsServiceRequest> = bodies
        .iter()
        .map(|body| {
            ExportLogsServiceRequest::decode(body.as_slice()).expect("decode OTLP logs request")
        })
        .collect();

    let mut records = Vec::new();
    for request in &requests {
        for resource_logs in &request.resource_logs {
            let resource = resource_logs.resource.as_ref().expect("resource present");
            assert!(
                attribute(&resource.attributes, "service.name")
                    .and_then(string_of)
                    .is_some_and(|name| name == "logs-cross-artifact"),
                "the SDK resource must carry the configured service.name"
            );
            for scope_logs in &resource_logs.scope_logs {
                let scope = scope_logs.scope.as_ref().expect("scope present");
                // The bridge deliberately does not expose `target`, which upstream uses to
                // OVERRIDE the scope name; the caller's scope must survive verbatim.
                assert_eq!(scope.name, "cross-artifact-scope");
                assert_eq!(scope.version, "1.2.3");
                records.extend(scope_logs.log_records.iter().cloned());
            }
        }
    }
    assert_eq!(
        records.len(),
        2,
        "exactly the two valid records must be exported (the invalid ones emit nothing)"
    );

    let detailed = records
        .iter()
        .find(|record| record.severity_number == 17)
        .expect("the ERROR record must be present");

    // Severity text is synthesized from the number: the pinned Rust setter takes a
    // `&'static str`, so it can never be caller-provided.
    assert_eq!(detailed.severity_text, "ERROR");
    // The phase-1 limitation, asserted rather than assumed: no event name on the wire.
    assert!(
        detailed.event_name.is_empty(),
        "event_name is deliberately not exposed by this bridge, but was set to {:?}",
        detailed.event_name
    );
    assert_eq!(detailed.time_unix_nano, 1_700_000_000_000_000_000);
    assert!(
        detailed.observed_time_unix_nano > 0,
        "the SDK must fill observed_timestamp when the caller leaves it unset"
    );
    assert_eq!(
        detailed.trace_id,
        (1u8..=16).collect::<Vec<u8>>(),
        "explicit trace context must reach the exported record"
    );
    assert_eq!(detailed.span_id, (0x10u8..0x18).collect::<Vec<u8>>());
    assert_eq!(detailed.flags, 1, "the SAMPLED flag must be preserved");
    assert_eq!(
        string_of(detailed.body.as_ref().expect("body present")),
        Some("cross artifact log body")
    );

    assert!(
        matches!(
            attribute(&detailed.attributes, "ratio").and_then(|v| v.value.as_ref()),
            Some(any_value::Value::DoubleValue(d)) if (*d - 0.5).abs() < f64::EPSILON
        ),
        "double attributes must round-trip"
    );

    // Duplicate top-level attribute keys are preserved, matching the pinned record.
    let duplicates: Vec<&str> = detailed
        .attributes
        .iter()
        .filter(|kv| kv.key == "dup")
        .filter_map(|kv| kv.value.as_ref().and_then(string_of))
        .collect();
    assert_eq!(
        duplicates,
        vec!["first", "second"],
        "duplicate top-level attribute keys must be preserved in order"
    );

    // The nested map/array/bytes structure must survive the flat node-pool conversion.
    let structured = attribute(&detailed.attributes, "structured").expect("structured attribute");
    let map = match structured.value.as_ref().expect("map value") {
        any_value::Value::KvlistValue(list) => list,
        other => panic!("expected a map value, got {other:?}"),
    };
    assert_eq!(map.values.len(), 2);
    assert!(
        matches!(
            attribute(&map.values, "flag").and_then(|v| v.value.as_ref()),
            Some(any_value::Value::BoolValue(true))
        ),
        "nested bool must round-trip"
    );
    let inner = match attribute(&map.values, "inner")
        .and_then(|v| v.value.as_ref())
        .expect("inner array")
    {
        any_value::Value::ArrayValue(array) => array,
        other => panic!("expected an array value, got {other:?}"),
    };
    assert_eq!(inner.values.len(), 2);
    assert!(matches!(
        inner.values[0].value.as_ref(),
        Some(any_value::Value::IntValue(7))
    ));
    assert!(
        matches!(
            inner.values[1].value.as_ref(),
            Some(any_value::Value::BytesValue(bytes)) if bytes.as_slice() == [0xDE, 0xAD]
        ),
        "bytes must round-trip without a UTF-8 requirement"
    );

    // The minimal record: no body, no attributes, severity synthesized all the same.
    let minimal = records
        .iter()
        .find(|record| record.severity_number == 9)
        .expect("the INFO record must be present");
    assert_eq!(minimal.severity_text, "INFO");
    assert!(
        minimal.body.is_none()
            || minimal
                .body
                .as_ref()
                .and_then(|b| b.value.as_ref())
                .is_none(),
        "an absent body must stay absent rather than becoming an empty string"
    );
    assert!(minimal.attributes.is_empty());
    assert!(
        minimal.observed_time_unix_nano > 0,
        "observed_timestamp must be defaulted even for a bare record"
    );
}

#[test]
fn api_only_log_emission_after_sdk_install_exports_through_the_sdk() {
    if !cfg!(unix) {
        eprintln!("skipping: the cross-artifact proof requires Unix dynamic linking");
        return;
    }
    let cc = match find_cc() {
        Some(cc) => cc,
        None => {
            if is_ci() {
                panic!(
                    "CI=true but no C compiler was found: the Logs cross-artifact proof cannot \
                     run. Install a C compiler or set the CC environment variable."
                );
            }
            eprintln!("skipping: no C compiler (set CC to enable)");
            return;
        }
    };
    let lib_dir = match find_lib_dir() {
        Some(dir) => dir,
        None => {
            if is_ci() {
                panic!(
                    "CI=true but the cdylibs are not built: the Logs cross-artifact proof cannot \
                     run. Build them first with: \
                     `cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk`."
                );
            }
            eprintln!(
                "skipping: cdylibs not built. Run: cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk"
            );
            return;
        }
    };

    let unique = format!(
        "otel_c_logs_cross_artifact_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let out = std::env::temp_dir().join(unique);
    let src = out.with_extension("c");
    std::fs::write(&src, HARNESS_C).expect("write harness source");

    let api_include = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("api/include");
    let sdk_include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let mut command = Command::new(&cc);
    command
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(&api_include)
        .arg("-I")
        .arg(&sdk_include)
        .arg(&src)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lopentelemetry_c_api")
        .arg("-lopentelemetry_c_sdk")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&out);
    if let Ok(flags) = std::env::var("CFLAGS") {
        command.args(flags.split_whitespace());
    }
    let compile = command.output().expect("invoke cc");
    assert!(
        compile.status.success(),
        "logs harness failed to compile/link:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let collector = start_mock();
    let endpoint = format!("http://127.0.0.1:{}/v1/logs", collector.port);
    let run = Command::new(&out)
        .env("OTEL_TEST_LOGS_ENDPOINT", &endpoint)
        .env("DYLD_LIBRARY_PATH", &lib_dir)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .expect("run logs harness");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while collector.bodies.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    collector.stop.store(true, Ordering::Release);
    collector.thread.join().expect("join mock collector");

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(
        run.status.success(),
        "logs harness exited with failure ({:?}):\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let bodies = collector.bodies.lock().unwrap();
    assert!(
        !bodies.is_empty(),
        "the mock collector received no OTLP log requests — API-only emission after SDK \
         install did NOT reach the SDK across the artifact boundary"
    );
    assert_exported_logs(&bodies);
    eprintln!(
        "logs cross-artifact export OK: {} OTLP request(s) via the API-only path",
        bodies.len()
    );
}
