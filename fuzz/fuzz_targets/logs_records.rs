//! Structured fuzzing of the Logs record surface.
//!
//! The interesting attack surface is the **flat value node pool**: a record hands the SDK an
//! array of nodes plus index ranges, and the SDK must prove every range is in bounds, forward,
//! acyclic, referenced exactly once, and within the depth/size budgets *before* it converts
//! anything. This target drives that validator with adversarial index ranges, tags, reserved
//! words, and structure sizes.
//!
//! Safety discipline: the fuzzer never supplies a raw address. Every pointer handed across the
//! ABI is either NULL or points at a live Rust buffer owned by this function; only lengths,
//! tags, indices, and structure sizes are fuzzer-controlled. A NULL pointer with a non-zero
//! length is deliberately generated because the implementation must reject it before any
//! dereference — that is the property under test, not undefined behavior.

#![no_main]

use std::os::raw::c_char;
use std::ptr;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{OtelBool, OtelLogsVtable, OtelScopeConfig, OTEL_LOGS_IMPL_ABI_VERSION};
use opentelemetry_c_api::{
    otel_api_logger_provider_new, otel_api_register_global_logger_provider_with_token,
    otel_global_logger_provider, otel_logger_destroy, otel_logger_emit, otel_logger_enabled,
    otel_logger_provider_destroy, otel_logger_provider_get_logger_with_options, OtelKeyValue,
    OtelLogBytesView, OtelLogRecordView, OtelLogTraceContext, OtelLogValue, OtelLogValueNode,
    OtelLogValuePayload, OtelLogValueRange, OtelLoggerOptions, OtelStatus, OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_sdk_build, otel_sdk_builder_destroy, otel_sdk_builder_new, otel_sdk_destroy,
    otel_sdk_logs_shutdown, otel_sdk_set_logs_as_global, OtelSdk, OtelSdkBuilder,
};

const MAX_STRING: usize = 64;
const MAX_NODES: usize = 24;
const MAX_ATTRIBUTES: usize = 12;

#[derive(Arbitrary, Debug)]
struct NodeSpec {
    key_mode: u8,
    value_type: u32,
    reserved: u32,
    first: u32,
    count: u32,
    number: u64,
}

#[derive(Arbitrary, Debug)]
struct VtableSpec {
    abi_version: u32,
    struct_size: usize,
    use_null: bool,
    register: bool,
}

#[derive(Arbitrary, Debug)]
struct Input {
    vtable: VtableSpec,
    text: Vec<u8>,
    raw_bytes: Vec<u8>,
    scope_name: Vec<u8>,
    string_mode: u8,
    record_struct_size: u64,
    options_struct_size: u64,
    severity: u32,
    present_fields: u64,
    reserved_flags: u32,
    reserved: [u64; 4],
    timestamp: u64,
    observed_timestamp: u64,
    trace_flags: u8,
    trace_reserved: u32,
    body: NodeSpec,
    nodes: Vec<NodeSpec>,
    attributes: Vec<NodeSpec>,
    node_count_mode: u8,
    attribute_count_mode: u8,
}

/// A structurally complete Logs vtable whose slots are never expected to be called: any
/// fuzzer-chosen header that reaches an indirect call is a bug, and these bodies make that
/// observable as a deliberate abort rather than as silent corruption.
extern "C" fn unreachable_get_logger(
    _: *mut std::ffi::c_void,
    _: *const OtelScopeConfig,
) -> *mut std::ffi::c_void {
    panic!("an incompatible logs vtable reached provider_get_logger");
}

extern "C" fn unreachable_retain(_: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    panic!("an incompatible logs vtable reached provider_retain");
}

extern "C" fn tracked_free(ctx: *mut std::ffi::c_void) {
    if !ctx.is_null() {
        drop(unsafe { Box::from_raw(ctx.cast::<u64>()) });
    }
}

extern "C" fn unreachable_enabled(_: *mut std::ffi::c_void, _: u32) -> OtelBool {
    panic!("an incompatible logs vtable reached logger_enabled");
}

extern "C" fn unreachable_emit(
    _: *mut std::ffi::c_void,
    _: *const OtelLogRecordView,
) -> OtelStatus {
    panic!("an incompatible logs vtable reached logger_emit");
}

/// Drive the vtable-header compatibility check with fuzzer-chosen ABI identifiers and sizes.
///
/// The registration contract is the interesting part: caller-owned context must transfer only
/// on success. Leaking on the rejection path or freeing on the success path would both show up
/// here, because the context is a real allocation freed by exactly one `provider_free`.
fn exercise_vtable(spec: &VtableSpec) {
    let vtable = OtelLogsVtable {
        // Nudge the fuzzer toward the accept/reject boundary without pinning it there.
        abi_version: match spec.abi_version % 4 {
            0 => OTEL_LOGS_IMPL_ABI_VERSION,
            1 => OTEL_LOGS_IMPL_ABI_VERSION.wrapping_add(1),
            2 => spec.abi_version,
            _ => 0,
        },
        struct_size: match spec.struct_size % 4 {
            0 => std::mem::size_of::<OtelLogsVtable>(),
            1 => std::mem::size_of::<OtelLogsVtable>() + 8,
            2 => 0,
            _ => spec.struct_size,
        },
        provider_get_logger: unreachable_get_logger,
        provider_retain: unreachable_retain,
        provider_free: tracked_free,
        logger_enabled: unreachable_enabled,
        logger_emit: unreachable_emit,
        logger_free: tracked_free,
    };
    let vtable_ptr = if spec.use_null {
        ptr::null()
    } else {
        &vtable as *const OtelLogsVtable
    };

    let ctx = Box::into_raw(Box::new(0xDEAD_BEEFu64)).cast::<std::ffi::c_void>();
    if spec.register {
        let mut id = 0u64;
        let status = unsafe {
            otel_api_register_global_logger_provider_with_token(vtable_ptr, ctx, &mut id)
        };
        if status == OtelStatus::Ok {
            // The slot took ownership; give it straight back so the vtable, which lives on this
            // stack frame, cannot outlive the registration.
            assert_ne!(id, 0);
            assert_eq!(
                opentelemetry_c_api::otel_api_unregister_global_logger_provider(id),
                OtelStatus::Ok
            );
        } else {
            // Rejected: the context is still ours and must be released exactly once.
            tracked_free(ctx);
        }
    } else {
        let provider = unsafe { otel_api_logger_provider_new(vtable_ptr, ctx) };
        if provider.is_null() {
            tracked_free(ctx);
        } else {
            unsafe { otel_logger_provider_destroy(provider) };
        }
    }
}

/// Build a string view over a live buffer. Mode 2 yields NULL with a non-zero length, which
/// the implementation must reject without dereferencing.
fn view(bytes: &[u8], mode: u8) -> OtelStringView {
    let bytes = &bytes[..bytes.len().min(MAX_STRING)];
    match mode % 4 {
        0 => OtelStringView {
            ptr: bytes.as_ptr().cast::<c_char>(),
            len: bytes.len(),
        },
        1 => OtelStringView::empty(),
        2 => OtelStringView {
            ptr: ptr::null(),
            len: 1 + bytes.len(),
        },
        _ => OtelStringView {
            ptr: bytes.as_ptr().cast::<c_char>(),
            len: bytes.len().min(1),
        },
    }
}

fn bytes_view(bytes: &[u8], mode: u8) -> OtelLogBytesView {
    let bytes = &bytes[..bytes.len().min(MAX_STRING)];
    match mode % 3 {
        0 => OtelLogBytesView {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        },
        1 => OtelLogBytesView {
            ptr: ptr::null(),
            len: 0,
        },
        _ => OtelLogBytesView {
            ptr: ptr::null(),
            len: 1 + bytes.len(),
        },
    }
}

fn prefix_size(raw: u64, complete: usize) -> u64 {
    match raw % 5 {
        0 => complete as u64,
        1 => 0,
        2 => complete.saturating_sub(1) as u64,
        3 => complete.saturating_add(1) as u64,
        _ => raw,
    }
}

/// Turn one spec into a value. Container ranges are intentionally allowed to point anywhere,
/// including backwards and out of bounds, because rejecting those is the property under test.
fn value(spec: &NodeSpec, text: &[u8], raw: &[u8], string_mode: u8) -> OtelLogValue {
    let payload = match spec.value_type % 8 {
        1 => OtelLogValuePayload {
            string_value: view(text, string_mode),
        },
        2 => OtelLogValuePayload {
            bool_value: (spec.number & 1) as u32,
        },
        3 => OtelLogValuePayload {
            int64_value: spec.number as i64,
        },
        4 => OtelLogValuePayload {
            double_value: f64::from_bits(spec.number),
        },
        5 => OtelLogValuePayload {
            bytes_value: bytes_view(raw, string_mode),
        },
        6 | 7 => OtelLogValuePayload {
            children: OtelLogValueRange {
                first: spec.first,
                count: spec.count,
            },
        },
        // 0 == EMPTY, plus any value_type beyond the known set once `% 8` wraps.
        _ => OtelLogValuePayload {
            string_value: OtelStringView::empty(),
        },
    };
    OtelLogValue {
        value_type: spec.value_type,
        reserved: spec.reserved,
        value: payload,
    }
}

/// Install an SDK LoggerProvider with no processors, so emission exercises the full validation
/// and conversion path without any exporter I/O. Returns NULL if the SDK refuses to build.
fn install_sdk() -> *mut OtelSdk {
    let builder: *mut OtelSdkBuilder = otel_sdk_builder_new();
    if builder.is_null() {
        return ptr::null_mut();
    }
    let mut sdk: *mut OtelSdk = ptr::null_mut();
    let status = unsafe { otel_sdk_build(builder, &mut sdk) };
    unsafe { otel_sdk_builder_destroy(builder) };
    if status != opentelemetry_c_api::OtelStatus::Ok || sdk.is_null() {
        return ptr::null_mut();
    }
    unsafe { otel_sdk_set_logs_as_global(sdk) };
    sdk
}

fuzz_target!(|input: Input| {
    exercise_vtable(&input.vtable);

    let sdk = install_sdk();

    let scope_name = view(&input.scope_name, input.string_mode);
    let options = OtelLoggerOptions {
        struct_size: prefix_size(
            input.options_struct_size,
            std::mem::size_of::<OtelLoggerOptions>(),
        ),
        name: scope_name,
        version: view(&input.text, input.string_mode.wrapping_add(1)),
        schema_url: view(&input.raw_bytes, input.string_mode.wrapping_add(2)),
        attributes: ptr::null::<OtelKeyValue>(),
        attribute_count: 0,
    };

    let provider = otel_global_logger_provider();
    let logger = unsafe { otel_logger_provider_get_logger_with_options(provider, &options) };
    if !logger.is_null() {
        // `enabled` must tolerate every severity, including 0 and out-of-range values.
        let _ = unsafe { otel_logger_enabled(logger, input.severity) };

        let nodes: Vec<OtelLogValueNode> = input
            .nodes
            .iter()
            .take(MAX_NODES)
            .map(|spec| OtelLogValueNode {
                key: view(&input.text, spec.key_mode),
                value: value(spec, &input.text, &input.raw_bytes, spec.key_mode),
            })
            .collect();
        let attributes: Vec<OtelLogValueNode> = input
            .attributes
            .iter()
            .take(MAX_ATTRIBUTES)
            .map(|spec| OtelLogValueNode {
                key: view(&input.scope_name, spec.key_mode),
                value: value(spec, &input.text, &input.raw_bytes, spec.key_mode),
            })
            .collect();

        // Counts are fuzzed independently of the real buffer lengths, including `usize::MAX`,
        // but a mismatched count is always paired with a NULL pointer so no out-of-bounds read
        // can be requested of us. Rejecting the NULL/non-zero-count pair is the property.
        let (node_ptr, node_count) = match input.node_count_mode % 3 {
            0 => (nodes.as_ptr(), nodes.len()),
            1 => (ptr::null(), 0),
            _ => (ptr::null(), usize::MAX),
        };
        let (attribute_ptr, attribute_count) = match input.attribute_count_mode % 3 {
            0 => (attributes.as_ptr(), attributes.len()),
            1 => (ptr::null(), 0),
            _ => (ptr::null(), usize::MAX),
        };

        let mut trace_context = OtelLogTraceContext {
            trace_id: [0; 16],
            span_id: [0; 8],
            trace_flags: input.trace_flags,
            reserved: [0; 7],
        };
        for (index, slot) in trace_context.trace_id.iter_mut().enumerate() {
            *slot = input.timestamp.wrapping_add(index as u64) as u8;
        }
        for (index, slot) in trace_context.span_id.iter_mut().enumerate() {
            *slot = input.observed_timestamp.wrapping_add(index as u64) as u8;
        }
        // Exercise the reserved-must-be-zero rule from both sides.
        if input.trace_reserved % 2 == 1 {
            trace_context.reserved[0] = input.trace_reserved as u8;
        }

        let record = OtelLogRecordView {
            struct_size: prefix_size(
                input.record_struct_size,
                std::mem::size_of::<OtelLogRecordView>(),
            ),
            present_fields: input.present_fields,
            timestamp_unix_nanos: input.timestamp,
            observed_timestamp_unix_nanos: input.observed_timestamp,
            severity_number: input.severity,
            reserved_flags: input.reserved_flags,
            body: value(
                &input.body,
                &input.text,
                &input.raw_bytes,
                input.string_mode,
            ),
            attributes: attribute_ptr,
            attribute_count,
            value_nodes: node_ptr,
            value_node_count: node_count,
            trace_context,
            reserved: input.reserved,
        };

        // Whatever the input, this must return a status rather than crash, hang, or read out
        // of bounds — and must never retain a pointer into the buffers above.
        let _ = unsafe { otel_logger_emit(logger, &record) };

        // A NULL record and a NULL logger must also be rejected cleanly.
        let _ = unsafe { otel_logger_emit(logger, ptr::null()) };
        let _ = unsafe { otel_logger_emit(ptr::null(), &record) };

        unsafe { otel_logger_destroy(logger) };
    }
    unsafe { otel_logger_provider_destroy(provider) };

    if !sdk.is_null() {
        unsafe { otel_sdk_logs_shutdown(sdk, 0) };
        unsafe { otel_sdk_destroy(sdk) };
    }
});
