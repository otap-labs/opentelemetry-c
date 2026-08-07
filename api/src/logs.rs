//! Public OpenTelemetry Logs Bridge API.
//!
//! This module owns the C-visible logger provider and logger handles and the no-op default
//! behavior. All record validation beyond the outermost header happens in the SDK, where the
//! Rust conversion occurs, so the no-SDK path never inspects nested borrowed content and
//! never allocates.

use std::os::raw::c_void;

use opentelemetry_c_abi::{
    log_severity_is_valid, OtelHandleHeader, OtelKeyValue, OtelLogRecordView, OtelLogTraceContext,
    OtelLogsVtable, OtelScopeConfig, OtelStringView, OTEL_HANDLE_KIND_LOGGER,
    OTEL_HANDLE_KIND_LOGGER_PROVIDER, OTEL_LOG_FIELD_TRACE_CONTEXT,
};

use crate::context::current_data;
use crate::error::{clear_last_error, fail, has_last_error, set_last_error, OtelStatus};
use crate::handle::HasHandleHeader;
use crate::handle::{checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw};
use crate::logs_global::{retain_global_logs, GlobalLogsRetain};
use crate::metrics::validate_scope_attributes;
use crate::trace::OtelSpanContext;
use crate::OtelBool;

pub(crate) enum LoggerProviderInner {
    /// Resolves the API-owned global slot lazily, at logger-acquisition time.
    Global,
    /// A specific SDK-backed provider; owns exactly one provider reference.
    Backed {
        vtable: *const OtelLogsVtable,
        ctx: *mut c_void,
    },
}

/// Opaque `otel_logger_provider_t`.
#[repr(C)]
pub struct OtelLoggerProvider {
    header: OtelHandleHeader,
    inner: LoggerProviderInner,
}

impl OtelLoggerProvider {
    pub(crate) fn new(inner: LoggerProviderInner) -> Self {
        Self {
            header: OtelHandleHeader::new(Self::KIND),
            inner,
        }
    }
}

impl HasHandleHeader for OtelLoggerProvider {
    const KIND: u64 = OTEL_HANDLE_KIND_LOGGER_PROVIDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

/// Opaque `otel_logger_t`. A NULL `vtable` is the no-op logger.
#[repr(C)]
pub struct OtelLogger {
    header: OtelHandleHeader,
    vtable: *const OtelLogsVtable,
    ctx: *mut c_void,
}

impl HasHandleHeader for OtelLogger {
    const KIND: u64 = OTEL_HANDLE_KIND_LOGGER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

// SAFETY: both handles only ever reach SDK state through the concurrency-safe vtable, whose
// contexts are documented as safe for shared use from multiple threads.
unsafe impl Send for OtelLoggerProvider {}
unsafe impl Sync for OtelLoggerProvider {}
unsafe impl Send for OtelLogger {}
unsafe impl Sync for OtelLogger {}

const _: () = {
    fn assert_sync<T: Sync>() {}
    let _ = assert_sync::<OtelLoggerProvider>;
    let _ = assert_sync::<OtelLogger>;
};

/// Extensible instrumentation-scope options for logger acquisition.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLoggerOptions {
    pub struct_size: u64,
    pub name: OtelStringView,
    pub version: OtelStringView,
    pub schema_url: OtelStringView,
    pub attributes: *const OtelKeyValue,
    pub attribute_count: usize,
}

/// Frozen V1 layout. Entry points read only this prefix, so a future appended field cannot
/// make an older caller's shorter structure be read out of bounds.
#[repr(C)]
#[derive(Clone, Copy)]
struct OtelLoggerOptionsV1 {
    struct_size: u64,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
}

const OTEL_LOGGER_OPTIONS_V1_SIZE: u64 = std::mem::size_of::<OtelLoggerOptionsV1>() as u64;

/// Frozen V1 log-record layout. Keep this separate from the public append-only structure so a
/// future field cannot silently change the minimum readable prefix.
#[repr(C)]
struct OtelLogRecordViewV1 {
    struct_size: u64,
    present_fields: u64,
    timestamp_unix_nanos: u64,
    observed_timestamp_unix_nanos: u64,
    severity_number: u32,
    reserved_flags: u32,
    body: opentelemetry_c_abi::OtelLogValue,
    attributes: *const opentelemetry_c_abi::OtelLogValueNode,
    attribute_count: usize,
    value_nodes: *const opentelemetry_c_abi::OtelLogValueNode,
    value_node_count: usize,
    trace_context: OtelLogTraceContext,
    reserved: [u64; 4],
}

/// Minimum accepted `otel_log_record_view_t::struct_size`.
pub(crate) const OTEL_LOG_RECORD_VIEW_V1_SIZE: u64 =
    std::mem::size_of::<OtelLogRecordViewV1>() as u64;

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelLoggerOptions>() == 72);
    assert!(std::mem::align_of::<OtelLoggerOptions>() == 8);
    assert!(OTEL_LOG_RECORD_VIEW_V1_SIZE == 160);
};
const _: () =
    assert!(std::mem::size_of::<OtelLogRecordView>() == std::mem::size_of::<OtelLogRecordViewV1>());

fn fail_abi(error: opentelemetry_c_abi::AbiError) -> OtelStatus {
    fail(error.status, error.message)
}

/// Obtain an owned logger from a provider.
///
/// # Safety
///
/// `provider` must be a live provider handle. Every non-empty string view must address
/// readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_provider_get_logger(
    provider: *const OtelLoggerProvider,
    name: OtelStringView,
    version: OtelStringView,
    schema_url: OtelStringView,
) -> *mut OtelLogger {
    let options = OtelLoggerOptions {
        struct_size: std::mem::size_of::<OtelLoggerOptions>() as u64,
        name,
        version,
        schema_url,
        attributes: std::ptr::null(),
        attribute_count: 0,
    };
    // SAFETY: forwarded caller contract; `options` is a live local.
    unsafe { otel_logger_provider_get_logger_with_options(provider, &options) }
}

/// Obtain an owned logger from complete instrumentation-scope options.
///
/// # Safety
///
/// `provider` must be live. `options` must be readable for its declared prefix, every
/// non-empty string must address readable bytes, and a non-empty attribute array must
/// contain `attribute_count` readable values for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_provider_get_logger_with_options(
    provider: *const OtelLoggerProvider,
    options: *const OtelLoggerOptions,
) -> *mut OtelLogger {
    guard_ptr(|| {
        clear_last_error();
        let provider = match unsafe { checked_ref(provider) } {
            Some(provider) => provider,
            None => return std::ptr::null_mut(),
        };
        if options.is_null() {
            fail(
                OtelStatus::InvalidArgument,
                "logger options must not be NULL",
            );
            return std::ptr::null_mut();
        }
        // Read only the stable `struct_size` prefix before forming any typed reference.
        let struct_size = unsafe { options.cast::<u64>().read() };
        if struct_size < OTEL_LOGGER_OPTIONS_V1_SIZE {
            fail(
                OtelStatus::InvalidArgument,
                "logger options struct_size is smaller than the supported prefix",
            );
            return std::ptr::null_mut();
        }
        let options = unsafe { &*options.cast::<OtelLoggerOptionsV1>() };
        let name = match unsafe { options.name.as_str() } {
            Ok(name) => name,
            Err(err) => {
                fail_abi(err);
                return std::ptr::null_mut();
            }
        };
        if name.is_empty() {
            fail(
                OtelStatus::InvalidConfig,
                "logger instrumentation name must not be empty",
            );
            return std::ptr::null_mut();
        }
        if let Err(err) = unsafe {
            options
                .version
                .as_str()
                .and_then(|_| options.schema_url.as_str())
        } {
            fail_abi(err);
            return std::ptr::null_mut();
        }
        // Validate everything before transferring anything to the SDK.
        if unsafe { validate_scope_attributes(options.attributes, options.attribute_count) }
            .is_err()
        {
            return std::ptr::null_mut();
        }
        let (vtable, ctx, owned) = match &provider.inner {
            LoggerProviderInner::Global => match retain_global_logs() {
                GlobalLogsRetain::NoProvider => {
                    return into_raw(OtelLogger {
                        header: OtelHandleHeader::new(OtelLogger::KIND),
                        vtable: std::ptr::null(),
                        ctx: std::ptr::null_mut(),
                    });
                }
                GlobalLogsRetain::RetainFailed => return std::ptr::null_mut(),
                GlobalLogsRetain::Retained { vtable, ctx } => (vtable, ctx, true),
            },
            LoggerProviderInner::Backed { vtable, ctx } => (*vtable, *ctx, false),
        };
        let scope = OtelScopeConfig {
            name: options.name,
            version: options.version,
            schema_url: options.schema_url,
            attributes: options.attributes,
            attribute_count: options.attribute_count,
        };
        let logger_ctx = unsafe { ((*vtable).provider_get_logger)(ctx, &scope) };
        if owned {
            unsafe { ((*vtable).provider_free)(ctx) };
        }
        if logger_ctx.is_null() {
            if !has_last_error() {
                set_last_error("backed logger creation failed");
            }
            return std::ptr::null_mut();
        }
        into_raw(OtelLogger {
            header: OtelHandleHeader::new(OtelLogger::KIND),
            vtable,
            ctx: logger_ctx,
        })
    })
}

/// Destroy a logger provider handle.
///
/// # Safety
///
/// `provider` must be NULL or a live provider handle, and destruction must not race with
/// another use of the same handle.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_provider_destroy(provider: *mut OtelLoggerProvider) {
    guard_unit(|| {
        if let Some(provider) = unsafe { checked_ref::<OtelLoggerProvider>(provider) } {
            if let LoggerProviderInner::Backed { vtable, ctx } = &provider.inner {
                unsafe { ((**vtable).provider_free)(*ctx) };
            }
        }
        unsafe { destroy(provider) };
    });
}

/// Destroy a logger handle.
///
/// # Safety
///
/// `logger` must be NULL or a live logger handle, and destruction must not race with another
/// use of the same handle.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_destroy(logger: *mut OtelLogger) {
    guard_unit(|| {
        if let Some(logger) = unsafe { checked_ref::<OtelLogger>(logger) } {
            if !logger.vtable.is_null() {
                unsafe { ((*logger.vtable).logger_free)(logger.ctx) };
            }
        }
        unsafe { destroy(logger) };
    });
}

/// Advisory check for whether a record of `severity` would be processed.
///
/// Returns `OTEL_FALSE` for a no-SDK logger and for any severity the pinned Rust check
/// cannot represent (absent `0`, or anything above 24). The result is advisory only:
/// configuration may change immediately after it returns, and
/// [`otel_logger_emit`] remains valid regardless.
///
/// # Safety
///
/// `logger` must be NULL or a live logger handle not destroyed concurrently.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_enabled(logger: *const OtelLogger, severity: u32) -> OtelBool {
    crate::handle::guard_value(0, || {
        clear_last_error();
        let logger = match unsafe { checked_ref::<OtelLogger>(logger) } {
            Some(logger) => logger,
            None => return 0,
        };
        if severity == 0 || !log_severity_is_valid(severity) {
            // The pinned Rust `event_enabled` requires a normalized severity, so there is
            // nothing meaningful to ask about. Report "not enabled" rather than inventing a
            // mapping.
            set_last_error("otel_logger_enabled requires a severity in 1..=24");
            return 0;
        }
        if logger.vtable.is_null() {
            return 0;
        }
        unsafe { ((*logger.vtable).logger_enabled)(logger.ctx, severity) }
    })
}

/// Emit one borrowed record.
///
/// The API layer validates only the logger handle and the record's readable header. The full
/// structural validation (presence bits, reserved fields, severity, node pool, limits, UTF-8)
/// runs in the SDK where the owned Rust record is constructed. Consequently, malformed nested
/// content is **not** diagnosed on the no-SDK path, which inspects no nested data at all.
///
/// # Safety
///
/// `logger` must be NULL or a live logger handle. `record` must be NULL or readable for its
/// declared `struct_size`, and every pointer reachable from it must stay valid for the
/// duration of this call.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_emit(
    logger: *const OtelLogger,
    record: *const OtelLogRecordView,
) -> OtelStatus {
    guard_status(|| {
        let logger = match unsafe { checked_ref::<OtelLogger>(logger) } {
            Some(logger) => logger,
            None => return OtelStatus::InvalidArgument,
        };
        if record.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view must not be NULL",
            );
        }
        // Only the stable `struct_size` prefix is read before any further access.
        let struct_size = unsafe { record.cast::<u64>().read() };
        if struct_size < OTEL_LOG_RECORD_VIEW_V1_SIZE {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view struct_size is smaller than the supported prefix",
            );
        }
        if logger.vtable.is_null() {
            // No SDK: allocation-free success. Nothing nested is read.
            return OtelStatus::Ok;
        }
        clear_last_error();
        unsafe { ((*logger.vtable).logger_emit)(logger.ctx, record) }
    })
}

/// Emit one record correlated with an immutable API-owned span-context snapshot.
///
/// # Safety
/// `logger` and `context` must be live and not destroyed concurrently. `record` and every
/// nested borrowed pointer must remain readable until this call returns.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_emit_with_context(
    logger: *const OtelLogger,
    record: *const OtelLogRecordView,
    context: *const OtelSpanContext,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let context = match unsafe { checked_ref(context) } {
            Some(context) if context.is_valid() => context,
            Some(_) => {
                return fail(OtelStatus::InvalidArgument, "span context is invalid");
            }
            None => return OtelStatus::InvalidArgument,
        };
        if record.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view must not be NULL",
            );
        }
        let struct_size = unsafe { record.cast::<u64>().read() };
        if struct_size < OTEL_LOG_RECORD_VIEW_V1_SIZE {
            return fail(
                OtelStatus::InvalidArgument,
                "log record view struct_size is smaller than the supported prefix",
            );
        }
        let mut correlated = unsafe { *record };
        // `correlated` contains only the V1 prefix even when a newer caller supplied a larger
        // record. Do not advertise unreadable trailing bytes to a newer SDK implementation.
        correlated.struct_size = OTEL_LOG_RECORD_VIEW_V1_SIZE;
        if correlated.present_fields & OTEL_LOG_FIELD_TRACE_CONTEXT != 0 {
            return fail(
                OtelStatus::InvalidArgument,
                "raw trace context and span-context handle are mutually exclusive",
            );
        }
        let context = context.view();
        correlated.present_fields |= OTEL_LOG_FIELD_TRACE_CONTEXT;
        correlated.trace_context = OtelLogTraceContext {
            trace_id: context.trace_id,
            span_id: context.span_id,
            trace_flags: context.trace_flags,
            reserved: [0; 7],
        };
        // SAFETY: `correlated` is a live local; all nested pointers still borrow from `record`.
        unsafe { otel_logger_emit(logger, &correlated) }
    })
}

/// Emit with correlation from the current API-owned C context when the record does not
/// already carry explicit trace context. Existing `otel_logger_emit` never consults TLS.
///
/// # Safety
/// `logger` must be live and `record` plus nested borrowed data must remain readable for the call.
#[no_mangle]
pub unsafe extern "C" fn otel_logger_emit_with_current_context(
    logger: *const OtelLogger,
    record: *const OtelLogRecordView,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let Some(logger_ref) = (unsafe { checked_ref::<OtelLogger>(logger) }) else {
            return OtelStatus::InvalidArgument;
        };
        if record.is_null() {
            return fail(OtelStatus::InvalidArgument, "log record is NULL");
        }
        let struct_size = unsafe { record.cast::<u64>().read() };
        if struct_size < OTEL_LOG_RECORD_VIEW_V1_SIZE {
            return fail(
                OtelStatus::InvalidConfig,
                "log record struct_size is too small",
            );
        }
        let present_fields = unsafe { std::ptr::addr_of!((*record).present_fields).read() };
        if logger_ref.vtable.is_null() || present_fields & OTEL_LOG_FIELD_TRACE_CONTEXT != 0 {
            return unsafe { otel_logger_emit(logger, record) };
        }
        let Some(data) = current_data().and_then(|context| context.span_context.as_ref().cloned())
        else {
            return unsafe { otel_logger_emit(logger, record) };
        };
        let context = OtelSpanContext::from_data(data);
        unsafe { otel_logger_emit_with_context(logger, record, &context) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_c_abi::{
        OtelLogValue, OtelLogValuePayload, OtelLogValueType, OTEL_LOGS_IMPL_ABI_VERSION,
    };

    extern "C" fn unused_get_logger(_: *mut c_void, _: *const OtelScopeConfig) -> *mut c_void {
        std::ptr::null_mut()
    }
    extern "C" fn unused_retain(_: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }
    extern "C" fn unused_free(_: *mut c_void) {}
    extern "C" fn unused_enabled(_: *mut c_void, _: u32) -> OtelBool {
        0
    }
    extern "C" fn capture_emit(_: *mut c_void, record: *const OtelLogRecordView) -> OtelStatus {
        let record = unsafe { &*record };
        assert_eq!(record.struct_size, OTEL_LOG_RECORD_VIEW_V1_SIZE);
        assert_ne!(record.present_fields & OTEL_LOG_FIELD_TRACE_CONTEXT, 0);
        assert_eq!(record.trace_context.trace_id, [1; 16]);
        assert_eq!(record.trace_context.span_id, [2; 8]);
        assert_eq!(record.trace_context.trace_flags, 3);
        OtelStatus::Ok
    }

    static TEST_VTABLE: OtelLogsVtable = OtelLogsVtable {
        abi_version: OTEL_LOGS_IMPL_ABI_VERSION,
        struct_size: std::mem::size_of::<OtelLogsVtable>(),
        provider_get_logger: unused_get_logger,
        provider_retain: unused_retain,
        provider_free: unused_free,
        logger_enabled: unused_enabled,
        logger_emit: capture_emit,
        logger_free: unused_free,
    };

    fn record() -> OtelLogRecordView {
        OtelLogRecordView {
            struct_size: std::mem::size_of::<OtelLogRecordView>() as u64,
            present_fields: 0,
            timestamp_unix_nanos: 0,
            observed_timestamp_unix_nanos: 0,
            severity_number: 9,
            reserved_flags: 0,
            body: OtelLogValue {
                value_type: OtelLogValueType::Empty as u32,
                reserved: 0,
                value: OtelLogValuePayload {
                    string_value: OtelStringView::empty(),
                },
            },
            attributes: std::ptr::null(),
            attribute_count: 0,
            value_nodes: std::ptr::null(),
            value_node_count: 0,
            trace_context: OtelLogTraceContext {
                trace_id: [0; 16],
                span_id: [0; 8],
                trace_flags: 0,
                reserved: [0; 7],
            },
            reserved: [0; 4],
        }
    }

    #[test]
    fn emit_with_context_injects_correlation_and_rejects_two_sources() {
        let logger = into_raw(OtelLogger {
            header: OtelHandleHeader::new(OtelLogger::KIND),
            vtable: &TEST_VTABLE,
            ctx: std::ptr::null_mut(),
        });
        let context = into_raw(OtelSpanContext::from_parts(
            [1; 16],
            [2; 8],
            3,
            false,
            String::new(),
        ));
        let mut record = record();
        record.struct_size = OTEL_LOG_RECORD_VIEW_V1_SIZE + 64;
        let original_struct_size = record.struct_size;
        let original_present_fields = record.present_fields;
        let original_trace_context = record.trace_context;
        assert_eq!(
            unsafe { otel_logger_emit_with_context(logger, &record, context) },
            OtelStatus::Ok
        );
        assert_eq!(record.struct_size, original_struct_size);
        assert_eq!(record.present_fields, original_present_fields);
        assert_eq!(
            record.trace_context.trace_id,
            original_trace_context.trace_id
        );
        assert_eq!(record.trace_context.span_id, original_trace_context.span_id);
        assert_eq!(
            record.trace_context.trace_flags,
            original_trace_context.trace_flags
        );
        let ambient = unsafe { crate::context::otel_context_create(context) };
        let mut scope = crate::context::OtelContextScope {
            struct_size: std::mem::size_of::<crate::context::OtelContextScope>(),
            thread_token: 0,
            generation: 0,
            reserved: [0; 2],
        };
        assert_eq!(
            unsafe { crate::context::otel_context_attach(ambient, &mut scope) },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe { otel_logger_emit_with_current_context(logger, &record) },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe { crate::context::otel_context_scope_detach(&mut scope) },
            OtelStatus::Ok
        );
        unsafe { crate::context::otel_context_destroy(ambient) };
        record.present_fields |= OTEL_LOG_FIELD_TRACE_CONTEXT;
        assert_eq!(
            unsafe { otel_logger_emit_with_context(logger, &record, context) },
            OtelStatus::InvalidArgument
        );
        unsafe { otel_logger_destroy(logger) };
        unsafe { crate::trace::otel_span_context_destroy(context) };
    }
}
