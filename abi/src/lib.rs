//! Shared C-ABI definitions for the `opentelemetry-c` API/SDK split.
//!
//! This crate is an **internal** rlib linked statically into both the `opentelemetry-c-api`
//! and `opentelemetry-c-sdk` cdylibs. It deliberately contains:
//!
//! - only `#[repr(C)]` value types and the internal implementation [`OtelImplVtable`], and
//! - pure helper functions (no `#[no_mangle]` exports, no `static` global state),
//!
//! so that linking it into both libraries introduces no duplicate exported symbols and no
//! duplicate global provider state. The **single** global provider slot lives in the API
//! cdylib; the SDK registers into it across the C ABI (see the API crate).
//!
//! Fallible conversions return [`AbiError`] (a status plus a `'static` message) rather than
//! touching an error slot, because the thread-local diagnostic slot is owned by whichever
//! cdylib is calling; the caller records the message in its own slot.

#![allow(clippy::missing_safety_doc)]

use std::os::raw::{c_char, c_void};

/// Common prefix for every API/SDK-owned opaque C handle.
///
/// This is internal to the coordinated API/SDK implementation. It lets entry points inspect
/// only two fixed-width fields before forming a reference to the expected concrete handle.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtelHandleHeader {
    magic: u64,
    kind: u64,
}

const OTEL_HANDLE_MAGIC: u64 = 0x4F54_4C43_4844_4C31; // "OTLCHDL1"

impl OtelHandleHeader {
    /// Construct a live header for `kind`.
    pub const fn new(kind: u64) -> Self {
        Self {
            magic: OTEL_HANDLE_MAGIC,
            kind,
        }
    }

    /// Whether this is a live handle of the expected kind.
    pub const fn is_live_kind(self, kind: u64) -> bool {
        self.magic == OTEL_HANDLE_MAGIC && self.kind == kind
    }

    /// Whether this carries the live-handle magic, independent of kind.
    pub const fn is_live(self) -> bool {
        self.magic == OTEL_HANDLE_MAGIC
    }

    /// Stored globally unique handle kind.
    pub const fn kind(self) -> u64 {
        self.kind
    }

    /// Poison a consumed or about-to-be-destroyed handle.
    pub fn poison(&mut self) {
        self.magic = 0;
    }
}

// API handle kinds.
pub const OTEL_HANDLE_KIND_TRACER_PROVIDER: u64 = 0x0101;
pub const OTEL_HANDLE_KIND_TRACER: u64 = 0x0102;
pub const OTEL_HANDLE_KIND_SPAN: u64 = 0x0103;
pub const OTEL_HANDLE_KIND_SPAN_CONTEXT: u64 = 0x0104;
pub const OTEL_HANDLE_KIND_METER_PROVIDER: u64 = 0x0110;
pub const OTEL_HANDLE_KIND_METER: u64 = 0x0111;
pub const OTEL_HANDLE_KIND_COUNTER_U64: u64 = 0x0120;
pub const OTEL_HANDLE_KIND_COUNTER_F64: u64 = 0x0121;
pub const OTEL_HANDLE_KIND_UP_DOWN_COUNTER_I64: u64 = 0x0122;
pub const OTEL_HANDLE_KIND_UP_DOWN_COUNTER_F64: u64 = 0x0123;
pub const OTEL_HANDLE_KIND_GAUGE_U64: u64 = 0x0124;
pub const OTEL_HANDLE_KIND_GAUGE_I64: u64 = 0x0125;
pub const OTEL_HANDLE_KIND_GAUGE_F64: u64 = 0x0126;
pub const OTEL_HANDLE_KIND_HISTOGRAM_U64: u64 = 0x0127;
pub const OTEL_HANDLE_KIND_HISTOGRAM_F64: u64 = 0x0128;
pub const OTEL_HANDLE_KIND_BOUND_COUNTER_U64: u64 = 0x0129;
pub const OTEL_HANDLE_KIND_BOUND_COUNTER_F64: u64 = 0x012A;
pub const OTEL_HANDLE_KIND_BOUND_HISTOGRAM_U64: u64 = 0x012B;
pub const OTEL_HANDLE_KIND_BOUND_HISTOGRAM_F64: u64 = 0x012C;
pub const OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_U64: u64 = 0x0130;
pub const OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_F64: u64 = 0x0131;
pub const OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_I64: u64 = 0x0132;
pub const OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_F64: u64 = 0x0133;
pub const OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_U64: u64 = 0x0134;
pub const OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_I64: u64 = 0x0135;
pub const OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_F64: u64 = 0x0136;
pub const OTEL_HANDLE_KIND_LOGGER_PROVIDER: u64 = 0x0140;
pub const OTEL_HANDLE_KIND_LOGGER: u64 = 0x0141;

// SDK handle kinds. These share one namespace with API handles because callers may pass a
// live handle across the library boundary with the wrong static C type.
pub const OTEL_HANDLE_KIND_SDK_BUILDER: u64 = 0x0201;
pub const OTEL_HANDLE_KIND_SDK: u64 = 0x0202;
pub const OTEL_HANDLE_KIND_OTLP_TRACE_EXPORTER_BUILDER: u64 = 0x0210;
pub const OTEL_HANDLE_KIND_TRACE_EXPORTER: u64 = 0x0211;
pub const OTEL_HANDLE_KIND_BATCH_SPAN_PROCESSOR_BUILDER: u64 = 0x0212;
pub const OTEL_HANDLE_KIND_SPAN_PROCESSOR: u64 = 0x0213;
pub const OTEL_HANDLE_KIND_OTLP_METRIC_EXPORTER_BUILDER: u64 = 0x0220;
pub const OTEL_HANDLE_KIND_METRIC_EXPORTER: u64 = 0x0221;
pub const OTEL_HANDLE_KIND_PERIODIC_METRIC_READER_BUILDER: u64 = 0x0222;
pub const OTEL_HANDLE_KIND_PERIODIC_METRIC_READER: u64 = 0x0223;
pub const OTEL_HANDLE_KIND_METRIC_VIEW_BUILDER: u64 = 0x0224;
pub const OTEL_HANDLE_KIND_METRIC_VIEW: u64 = 0x0225;
pub const OTEL_HANDLE_KIND_MANUAL_METRIC_READER: u64 = 0x0226;
pub const OTEL_HANDLE_KIND_OTLP_LOG_EXPORTER_BUILDER: u64 = 0x0230;
pub const OTEL_HANDLE_KIND_LOG_EXPORTER: u64 = 0x0231;
pub const OTEL_HANDLE_KIND_BATCH_LOG_PROCESSOR_BUILDER: u64 = 0x0232;
pub const OTEL_HANDLE_KIND_LOG_PROCESSOR: u64 = 0x0233;

/// Fixed-width status code returned by fallible C API functions. Mirrors `otel_status_t`.
///
/// `Ok` (0) is success; any non-zero value is a failure. This is an integer newtype rather
/// than a Rust enum so receiving a status added by a newer API/SDK remains well-defined.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtelStatus(pub u32);

#[allow(non_upper_case_globals)]
impl OtelStatus {
    /// Operation completed successfully.
    pub const Ok: Self = Self(0);
    /// A required pointer argument was NULL, or a handle failed validation.
    pub const InvalidArgument: Self = Self(1);
    /// A string argument was not valid UTF-8 where UTF-8 is required.
    pub const InvalidUtf8: Self = Self(2);
    /// Readable configuration was incompatible, invalid as a whole, or requested support
    /// that was not compiled in.
    pub const InvalidConfig: Self = Self(3);
    /// The SDK (or provider) has already been shut down.
    pub const AlreadyShutdown: Self = Self(4);
    /// The operation did not complete within the supplied timeout.
    pub const Timeout: Self = Self(5);
    /// An exporter or callback-driven export pipeline failed at runtime.
    pub const ExportFailed: Self = Self(6);
    /// The C wrapper or SDK infrastructure failed unexpectedly, including a caught panic,
    /// allocation failure, or worker-thread creation failure.
    pub const InternalError: Self = Self(7);
}

/// A fallible-conversion error: a status code and a `'static` diagnostic message. The
/// caller records the message in its own thread-local error slot.
#[derive(Debug, Clone, Copy)]
pub struct AbiError {
    /// The status to return to C.
    pub status: OtelStatus,
    /// A human-readable diagnostic, recorded by the caller.
    pub message: &'static str,
}

impl AbiError {
    const fn new(status: OtelStatus, message: &'static str) -> Self {
        AbiError { status, message }
    }
}

/// A C boolean, passed across the FFI boundary as a raw 32-bit integer (`0` = false,
/// non-zero = true). An integer alias (not an enum) so every bit pattern is well-defined.
pub type OtelBool = u32;

/// A borrowed, length-delimited UTF-8 string (mirrors `otel_string_view_t`).
///
/// `ptr` may be NULL only when `len == 0`. The bytes need not be NUL-terminated and must
/// remain valid for the duration of the call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OtelStringView {
    /// Pointer to the first UTF-8 byte, or NULL when `len == 0`.
    pub ptr: *const c_char,
    /// Number of bytes referenced by `ptr`.
    pub len: usize,
}

impl OtelStringView {
    /// An empty view (NULL pointer, zero length).
    pub fn empty() -> Self {
        OtelStringView {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    /// Borrow the raw bytes. Rejects a NULL pointer with non-zero length, or a length
    /// exceeding `isize::MAX` (a `slice::from_raw_parts` safety precondition), before any
    /// slice is formed.
    ///
    /// # Safety
    /// `ptr` must be valid for reads of `len` bytes, or NULL when `len == 0`.
    unsafe fn as_bytes(&self) -> Result<&[u8], AbiError> {
        if self.len == 0 {
            return Ok(&[]);
        }
        if self.ptr.is_null() {
            return Err(AbiError::new(
                OtelStatus::InvalidArgument,
                "string view has NULL ptr with non-zero len",
            ));
        }
        if self.len > isize::MAX as usize {
            return Err(AbiError::new(
                OtelStatus::InvalidArgument,
                "string view len exceeds the maximum supported size",
            ));
        }
        // SAFETY: non-NULL, caller guarantees `len` valid bytes, `len <= isize::MAX`.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) })
    }

    /// Convert to a `&str`, requiring valid UTF-8.
    ///
    /// # Safety
    /// `ptr` must be valid for reads of `len` bytes, or NULL when `len == 0`.
    pub unsafe fn as_str(&self) -> Result<&str, AbiError> {
        // SAFETY: forwarded to the caller's contract.
        let bytes = unsafe { self.as_bytes() }?;
        std::str::from_utf8(bytes)
            .map_err(|_| AbiError::new(OtelStatus::InvalidUtf8, "string view is not valid UTF-8"))
    }

    /// Convert to an owned `String`, requiring valid UTF-8. The copy is fallible.
    ///
    /// # Safety
    /// `ptr` must be valid for reads of `len` bytes, or NULL when `len == 0`.
    pub unsafe fn to_string_strict(self) -> Result<String, AbiError> {
        // SAFETY: forwarded to the caller's contract.
        let s = unsafe { self.as_str() }?;
        try_owned_string(s)
    }

    /// Convert to an owned `String`, replacing invalid UTF-8 with U+FFFD. The copy is
    /// fallible (allocation failure yields `InternalError` instead of aborting).
    ///
    /// # Safety
    /// `ptr` must be valid for reads of `len` bytes, or NULL when `len == 0`.
    pub unsafe fn to_string_lossy(self) -> Result<String, AbiError> {
        // SAFETY: forwarded to the caller's contract.
        let bytes = unsafe { self.as_bytes() }?;
        try_owned_string_lossy(bytes)
    }
}

fn alloc_err() -> AbiError {
    AbiError::new(OtelStatus::InternalError, "failed to allocate string")
}

fn push_str_fallible(out: &mut String, s: &str) -> Result<(), AbiError> {
    out.try_reserve(s.len()).map_err(|_| alloc_err())?;
    out.push_str(s);
    Ok(())
}

/// Fallibly copy a `&str` into an owned `String`.
pub fn try_owned_string(s: &str) -> Result<String, AbiError> {
    let mut owned = String::new();
    push_str_fallible(&mut owned, s)?;
    Ok(owned)
}

/// Fallibly build the lossy (U+FFFD-replaced) owned form of `bytes`, matching
/// `String::from_utf8_lossy` but never allocating infallibly from C-controlled length.
pub fn try_owned_string_lossy(bytes: &[u8]) -> Result<String, AbiError> {
    let mut out = String::new();
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                push_str_fallible(&mut out, valid)?;
                return Ok(out);
            }
            Err(err) => {
                let good = err.valid_up_to();
                // SAFETY: `rest[..good]` is valid UTF-8 by the definition of `valid_up_to`.
                let valid = unsafe { std::str::from_utf8_unchecked(&rest[..good]) };
                push_str_fallible(&mut out, valid)?;
                push_str_fallible(&mut out, "\u{FFFD}")?;
                match err.error_len() {
                    Some(bad) => rest = &rest[good + bad..],
                    None => return Ok(out),
                }
            }
        }
    }
}

/// Discriminant identifying the active member of [`OtelAttributeValue`]. Crosses the ABI
/// as a raw `u32`; validated via [`OtelAttributeType::from_u32`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelAttributeType {
    /// `string_value` is active.
    String = 0,
    /// `bool_value` is active.
    Bool = 1,
    /// `int64_value` is active.
    Int64 = 2,
    /// `double_value` is active.
    Double = 3,
}

impl OtelAttributeType {
    /// Validate a raw tag, returning `None` for unknown values.
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(OtelAttributeType::String),
            1 => Some(OtelAttributeType::Bool),
            2 => Some(OtelAttributeType::Int64),
            3 => Some(OtelAttributeType::Double),
            _ => None,
        }
    }
}

/// Tagged-union payload for an attribute value.
#[repr(C)]
#[derive(Clone, Copy)]
pub union OtelAttributeValue {
    /// Active for [`OtelAttributeType::String`].
    pub string_value: OtelStringView,
    /// Active for [`OtelAttributeType::Bool`] (`0` = false, else true).
    pub bool_value: OtelBool,
    /// Active for [`OtelAttributeType::Int64`].
    pub int64_value: i64,
    /// Active for [`OtelAttributeType::Double`].
    pub double_value: f64,
}

/// A single typed attribute: a key plus a tagged value (mirrors `otel_key_value_t`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelKeyValue {
    /// Attribute key (UTF-8). Must not be empty.
    pub key: OtelStringView,
    /// Selects the active member of `value`; must be a valid [`OtelAttributeType`] tag.
    pub value_type: u32,
    /// The attribute value payload.
    pub value: OtelAttributeValue,
}

/// Span kind, mirroring `opentelemetry::trace::SpanKind`. Crosses the ABI as a raw `u32`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelSpanKind {
    /// Internal operation with no remote counterpart (default).
    Internal = 0,
    /// Handles a synchronous inbound request.
    Server = 1,
    /// Issues a synchronous outbound request.
    Client = 2,
    /// Initiates an asynchronous message.
    Producer = 3,
    /// Processes an asynchronous message.
    Consumer = 4,
}

impl OtelSpanKind {
    /// Validate a raw span-kind value, returning `None` if unknown.
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(OtelSpanKind::Internal),
            1 => Some(OtelSpanKind::Server),
            2 => Some(OtelSpanKind::Client),
            3 => Some(OtelSpanKind::Producer),
            4 => Some(OtelSpanKind::Consumer),
            _ => None,
        }
    }
}

/// Span status code, mirroring `opentelemetry::trace::Status`. Crosses the ABI as `u32`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelSpanStatusCode {
    /// Default, unset status.
    Unset = 0,
    /// Explicitly successful.
    Ok = 1,
    /// The operation contains an error.
    Error = 2,
}

impl OtelSpanStatusCode {
    /// Validate a raw status-code value, returning `None` if unknown.
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(OtelSpanStatusCode::Unset),
            1 => Some(OtelSpanStatusCode::Ok),
            2 => Some(OtelSpanStatusCode::Error),
            _ => None,
        }
    }
}

/// Metrics instrument kind. Crosses the ABI as a validated raw `u32`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelMetricInstrumentKind {
    Counter = 0,
    UpDownCounter = 1,
    Gauge = 2,
    Histogram = 3,
    ObservableCounter = 4,
    ObservableUpDownCounter = 5,
    ObservableGauge = 6,
}

impl OtelMetricInstrumentKind {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Counter),
            1 => Some(Self::UpDownCounter),
            2 => Some(Self::Gauge),
            3 => Some(Self::Histogram),
            4 => Some(Self::ObservableCounter),
            5 => Some(Self::ObservableUpDownCounter),
            6 => Some(Self::ObservableGauge),
            _ => None,
        }
    }
}

/// Metrics numeric type. Crosses the ABI as a validated raw `u32`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelMetricNumberKind {
    U64 = 0,
    I64 = 1,
    F64 = 2,
}

impl OtelMetricNumberKind {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::U64),
            1 => Some(Self::I64),
            2 => Some(Self::F64),
            _ => None,
        }
    }
}

/// Internal instrument creation configuration shared by API and SDK.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricInstrumentConfig {
    pub kind: u32,
    pub number: u32,
    pub name: OtelStringView,
    pub description: OtelStringView,
    pub unit: OtelStringView,
    pub boundaries: *const f64,
    pub boundary_count: usize,
    pub callback: Option<extern "C" fn(observer_ctx: *mut c_void, state: *mut c_void)>,
    /// For observable creation, ownership transfers to `meter_create_instrument` on entry
    /// when this pointer and `callback_state_free` are present, even if creation fails.
    pub callback_state: *mut c_void,
    pub callback_state_free: Option<extern "C" fn(state: *mut c_void)>,
}

/// Borrowed, implementation-neutral trace context snapshot used only during one vtable call.
///
/// The API copies this data into its own immutable `otel_span_context_t` handle. Conversely,
/// when starting a span from a snapshot, every pointer remains valid until the vtable call
/// returns. No Rust type or allocation ownership crosses the shared-library boundary.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelSpanContextView {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub trace_flags: u8,
    pub reserved: [u8; 3],
    pub is_remote: OtelBool,
    pub trace_state: OtelStringView,
}

pub type OtelSpanContextVisitor = Option<
    extern "C" fn(user_data: *mut c_void, context: *const OtelSpanContextView) -> OtelStatus,
>;

/// Internal implementation vtable registered by the SDK into the API-owned global slot
/// (and returned from `otel_sdk_get_tracer_provider`).
///
/// Every function is `extern "C"` and takes an opaque `*mut c_void` context that the SDK
/// allocated and only the SDK frees (via the `*_free` entries). No Rust types cross this
/// boundary. A single `'static` instance of this vtable lives in the SDK cdylib; the API
/// stores a `*const OtelImplVtable` in each handle it creates for an SDK-backed object.
///
/// ## Provider context ownership (reference counting)
///
/// A `provider_ctx` is **reference-counted**. Each `provider_ctx` value is one owned
/// reference that must be released with exactly one [`provider_free`](Self::provider_free).
/// [`provider_retain`](Self::provider_retain) produces a **new, independent** owned
/// reference to the same underlying provider (a cheap `Arc` clone in the SDK). This lets
/// the API resolve the process-global provider without a use-after-free: it retains the
/// slot's context *while holding the global read lock* (so a concurrent
/// `register`/replace — which needs the write lock — cannot free it mid-retain), releases
/// the lock, uses the retained reference, then frees it. A replacement frees only the
/// slot's own reference; retained references keep the provider alive independently.
///
/// Tracer and span contexts are single-owner (not reference-counted): each is created by
/// one call and freed by exactly one matching `*_free`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelImplVtable {
    /// Trace ABI kind/version identifier.
    pub abi_version: u32,
    /// Size of this vtable in bytes. New entries may only be appended.
    pub struct_size: usize,
    /// Create a tracer from a provider context. Returns an opaque tracer context.
    pub provider_get_tracer: extern "C" fn(
        provider_ctx: *mut c_void,
        name: OtelStringView,
        version: OtelStringView,
        schema_url: OtelStringView,
    ) -> *mut c_void,
    /// Produce a **new owned reference** to the same provider (an `Arc` clone in the SDK),
    /// or NULL on failure. The returned context must be released with one `provider_free`.
    /// Used by the API to safely snapshot the global provider across a lock boundary.
    pub provider_retain: extern "C" fn(provider_ctx: *mut c_void) -> *mut c_void,
    /// Release one owned reference to a provider context (see the ownership notes above).
    pub provider_free: extern "C" fn(provider_ctx: *mut c_void),

    /// Start a span from a tracer context. `parent_span_ctx` is NULL for a root span, or
    /// a span context previously produced by *this same vtable*. Returns a span context.
    pub tracer_start_span: extern "C" fn(
        tracer_ctx: *mut c_void,
        name: OtelStringView,
        kind: u32,
        parent_span_ctx: *mut c_void,
    ) -> *mut c_void,
    /// Free a tracer context.
    pub tracer_free: extern "C" fn(tracer_ctx: *mut c_void),

    /// Set a string attribute. Returns a status (e.g. invalid key/UTF-8/allocation).
    pub span_set_string: extern "C" fn(
        span_ctx: *mut c_void,
        key: OtelStringView,
        value: OtelStringView,
    ) -> OtelStatus,
    /// Set a boolean attribute (`0` = false, non-zero = true).
    pub span_set_bool:
        extern "C" fn(span_ctx: *mut c_void, key: OtelStringView, value: OtelBool) -> OtelStatus,
    /// Set an i64 attribute.
    pub span_set_i64:
        extern "C" fn(span_ctx: *mut c_void, key: OtelStringView, value: i64) -> OtelStatus,
    /// Set an f64 attribute.
    pub span_set_f64:
        extern "C" fn(span_ctx: *mut c_void, key: OtelStringView, value: f64) -> OtelStatus,
    /// Add a timestamped event with optional attributes.
    pub span_add_event: extern "C" fn(
        span_ctx: *mut c_void,
        name: OtelStringView,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    /// Set the span status (`code` is an `OtelSpanStatusCode` value).
    pub span_set_status:
        extern "C" fn(span_ctx: *mut c_void, code: u32, description: OtelStringView) -> OtelStatus,
    /// Rename the span.
    pub span_update_name: extern "C" fn(span_ctx: *mut c_void, name: OtelStringView) -> OtelStatus,
    /// End the span. The API guards idempotency (calls this at most once per span).
    pub span_end: extern "C" fn(span_ctx: *mut c_void),
    /// Free a span context. The API always ends the span (via `span_end`) before calling
    /// this; dropping the underlying span additionally ends it if it was somehow not ended,
    /// so no span is left unfinished (and no span is double-ended).
    pub span_free: extern "C" fn(span_ctx: *mut c_void),

    /// Visit a borrowed snapshot of the span's immutable context. Optional appended entry.
    pub span_context_visit: extern "C" fn(
        span_ctx: *mut c_void,
        visitor: OtelSpanContextVisitor,
        user_data: *mut c_void,
    ) -> OtelStatus,
    /// Start a span from an implementation-neutral parent context. Optional appended entry.
    pub tracer_start_span_with_context: extern "C" fn(
        tracer_ctx: *mut c_void,
        name: OtelStringView,
        kind: u32,
        parent: *const OtelSpanContextView,
    ) -> *mut c_void,
}

/// Current trace implementation ABI kind/version identifier.
pub const OTEL_TRACE_IMPL_ABI_VERSION: u32 = 1;

/// Compatibility alias for existing trace consumers.
///
/// Metrics vtables must use [`OTEL_METRICS_IMPL_ABI_VERSION`] instead.
pub const OTEL_IMPL_ABI_VERSION: u32 = OTEL_TRACE_IMPL_ABI_VERSION;

/// Minimum vtable size required by this API version.
pub const OTEL_IMPL_VTABLE_REQUIRED_SIZE: usize =
    std::mem::offset_of!(OtelImplVtable, span_context_visit);

/// Internal Metrics implementation vtable. Metrics uses a distinct ABI identifier,
/// provider context, and API-owned global slot from traces.
///
/// Signal-neutral instrumentation-scope configuration shared by Metrics and Logs. The
/// layout is unchanged from the original Metrics-only definition; the Metrics name remains
/// available as [`OtelMetricScopeConfig`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelScopeConfig {
    pub name: OtelStringView,
    pub version: OtelStringView,
    pub schema_url: OtelStringView,
    pub attributes: *const OtelKeyValue,
    pub attribute_count: usize,
}

/// Compatibility alias preserving the original Metrics name (identical layout).
pub type OtelMetricScopeConfig = OtelScopeConfig;

pub type OtelProviderGetMeterWithScope =
    extern "C" fn(provider_ctx: *mut c_void, scope: *const OtelMetricScopeConfig) -> *mut c_void;

pub type OtelMeterCreateInstrumentWithStatus = extern "C" fn(
    meter_ctx: *mut c_void,
    config: *const OtelMetricInstrumentConfig,
    out_status: *mut OtelStatus,
) -> *mut c_void;

pub type OtelMetricInstrumentBind = extern "C" fn(
    instrument_ctx: *mut c_void,
    attributes: *const OtelKeyValue,
    attribute_count: usize,
    out_status: *mut OtelStatus,
) -> *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelMetricsVtable {
    /// Metrics ABI kind/version identifier.
    pub abi_version: u32,
    /// Size of this vtable in bytes. New entries may only be appended.
    pub struct_size: usize,
    pub provider_get_meter: extern "C" fn(
        provider_ctx: *mut c_void,
        name: OtelStringView,
        version: OtelStringView,
        schema_url: OtelStringView,
    ) -> *mut c_void,
    pub provider_retain: extern "C" fn(provider_ctx: *mut c_void) -> *mut c_void,
    pub provider_free: extern "C" fn(provider_ctx: *mut c_void),
    pub meter_create_instrument: extern "C" fn(
        meter_ctx: *mut c_void,
        config: *const OtelMetricInstrumentConfig,
    ) -> *mut c_void,
    pub meter_free: extern "C" fn(meter_ctx: *mut c_void),
    pub instrument_record_u64: extern "C" fn(
        instrument_ctx: *mut c_void,
        value: u64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub instrument_record_i64: extern "C" fn(
        instrument_ctx: *mut c_void,
        value: i64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub instrument_record_f64: extern "C" fn(
        instrument_ctx: *mut c_void,
        value: f64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub observer_observe_u64: extern "C" fn(
        observer_ctx: *mut c_void,
        value: u64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub observer_observe_i64: extern "C" fn(
        observer_ctx: *mut c_void,
        value: i64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub observer_observe_f64: extern "C" fn(
        observer_ctx: *mut c_void,
        value: f64,
        attributes: *const OtelKeyValue,
        attribute_count: usize,
    ) -> OtelStatus,
    pub instrument_free: extern "C" fn(instrument_ctx: *mut c_void),
    /// Append-only complete-scope extension. Access only when `struct_size` is at least
    /// [`OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE`].
    pub provider_get_meter_with_scope: OtelProviderGetMeterWithScope,
    /// Append-only creation-status extension. Unlike the legacy NULL-only entry, this
    /// preserves the implementation's exact failure classification.
    pub meter_create_instrument_with_status: OtelMeterCreateInstrumentWithStatus,
    /// Append-only experimental bound-instrument extension. Access only when `struct_size`
    /// is at least [`OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE`].
    pub instrument_bind: OtelMetricInstrumentBind,
    pub bound_instrument_record_u64:
        extern "C" fn(bound_instrument_ctx: *mut c_void, value: u64) -> OtelStatus,
    pub bound_instrument_record_f64:
        extern "C" fn(bound_instrument_ctx: *mut c_void, value: f64) -> OtelStatus,
    pub bound_instrument_free: extern "C" fn(bound_instrument_ctx: *mut c_void),
}

/// Current Metrics implementation ABI kind/version identifier.
///
/// The high byte `0x4D` (`M`) reserves the Metrics namespace; the low 24 bits carry the
/// version within that namespace.
pub const OTEL_METRICS_IMPL_ABI_VERSION: u32 = 0x4D00_0001;

/// Historical Metrics vtable prefix size before append-only scope attributes were added.
#[cfg(target_pointer_width = "64")]
pub const OTEL_METRICS_VTABLE_REQUIRED_SIZE: usize = 112;
#[cfg(target_pointer_width = "32")]
pub const OTEL_METRICS_VTABLE_REQUIRED_SIZE: usize = 56;

/// Metrics vtable prefix size through [`OtelMetricScopeConfig`] support.
#[cfg(target_pointer_width = "64")]
pub const OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE: usize = 120;
#[cfg(target_pointer_width = "32")]
pub const OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE: usize = 60;

/// Metrics vtable prefix size through exact instrument-creation status support.
#[cfg(target_pointer_width = "64")]
pub const OTEL_METRICS_VTABLE_CREATION_STATUS_SIZE: usize = 128;
#[cfg(target_pointer_width = "32")]
pub const OTEL_METRICS_VTABLE_CREATION_STATUS_SIZE: usize = 64;

/// Metrics vtable prefix size through experimental bound-instrument support.
#[cfg(target_pointer_width = "64")]
pub const OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE: usize = 160;
#[cfg(target_pointer_width = "32")]
pub const OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE: usize = 80;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelVtableHeader {
    pub abi_version: u32,
    pub struct_size: usize,
}

/// Validate the stable prefix of a trace implementation vtable.
///
/// The ABI identifier selects both the trace signal kind and its compatible version.
/// `struct_size` permits append-only extensions within that same kind.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
/// If the header is compatible, it must also be readable as a complete [`OtelImplVtable`]
/// and remain live wherever the caller subsequently stores or uses it. No bytes beyond the
/// header are read by this function.
pub unsafe fn trace_vtable_compatible(vtable: *const OtelImplVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_TRACE_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_IMPL_VTABLE_REQUIRED_SIZE
}

/// Validate the stable prefix of a Metrics implementation vtable.
///
/// The ABI identifier selects both the Metrics signal kind and its compatible version.
/// `struct_size` permits append-only extensions within that same kind.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
/// If the header is compatible, it must also be readable through
/// [`OTEL_METRICS_VTABLE_REQUIRED_SIZE`] and remain live wherever the caller stores or uses
/// it. Append-only fields may be accessed only after their own size check. No bytes beyond
/// the header are read by this function.
pub unsafe fn metrics_vtable_compatible(vtable: *const OtelMetricsVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_METRICS_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_METRICS_VTABLE_REQUIRED_SIZE
}

/// Whether a compatible Metrics vtable includes the complete-scope extension.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
pub unsafe fn metrics_vtable_supports_scope_config(vtable: *const OtelMetricsVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_METRICS_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE
}

/// Whether a compatible Metrics vtable includes exact instrument-creation status.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
pub unsafe fn metrics_vtable_supports_creation_status(vtable: *const OtelMetricsVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_METRICS_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_METRICS_VTABLE_CREATION_STATUS_SIZE
}

/// Whether a compatible Metrics vtable includes experimental bound-instrument support.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
pub unsafe fn metrics_vtable_supports_bound_instruments(vtable: *const OtelMetricsVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_METRICS_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE
}

const _: () = assert!(OTEL_TRACE_IMPL_ABI_VERSION != OTEL_METRICS_IMPL_ABI_VERSION);
const _: () = assert!(OTEL_METRICS_IMPL_ABI_VERSION & 0xFF00_0000 == 0x4D00_0000);
const _: () = assert!(
    OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE
        == OTEL_METRICS_VTABLE_REQUIRED_SIZE + std::mem::size_of::<OtelProviderGetMeterWithScope>()
);
const _: () = assert!(
    OTEL_METRICS_VTABLE_CREATION_STATUS_SIZE
        == OTEL_METRICS_VTABLE_SCOPE_CONFIG_SIZE + std::mem::size_of::<*const c_void>()
);
const _: () = assert!(
    OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE
        == OTEL_METRICS_VTABLE_CREATION_STATUS_SIZE + 4 * std::mem::size_of::<*const c_void>()
);
const _: () =
    assert!(std::mem::size_of::<OtelMetricsVtable>() == OTEL_METRICS_VTABLE_BOUND_INSTRUMENT_SIZE);

// ---- Logs -------------------------------------------------------------------

/// Discriminant identifying the active member of [`OtelLogValuePayload`]. Crosses the ABI
/// as a raw `u32`; validated via [`OtelLogValueType::from_u32`].
///
/// This is the Logs `AnyValue` model, which is strictly wider than the scalar
/// [`OtelAttributeType`] model used by Trace and Metrics attributes.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelLogValueType {
    /// No value is present (absent body / empty value). No union member is read.
    Empty = 0,
    /// `string_value` is active. Must be valid UTF-8.
    String = 1,
    /// `bool_value` is active.
    Bool = 2,
    /// `int64_value` is active.
    Int64 = 3,
    /// `double_value` is active.
    Double = 4,
    /// `bytes_value` is active. Arbitrary bytes; no UTF-8 requirement.
    Bytes = 5,
    /// `children` is active and addresses array elements in the record's node pool.
    Array = 6,
    /// `children` is active and addresses map entries in the record's node pool.
    Map = 7,
}

impl OtelLogValueType {
    /// Validate a raw tag, returning `None` for unknown values.
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Empty),
            1 => Some(Self::String),
            2 => Some(Self::Bool),
            3 => Some(Self::Int64),
            4 => Some(Self::Double),
            5 => Some(Self::Bytes),
            6 => Some(Self::Array),
            7 => Some(Self::Map),
            _ => None,
        }
    }
}

/// A borrowed, length-delimited byte string (mirrors `otel_log_bytes_view_t`).
///
/// `ptr` may be NULL only when `len == 0`. Unlike [`OtelStringView`] the bytes are not
/// required to be valid UTF-8.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OtelLogBytesView {
    /// Pointer to the first byte, or NULL when `len == 0`.
    pub ptr: *const u8,
    /// Number of bytes referenced by `ptr`.
    pub len: usize,
}

impl OtelLogBytesView {
    /// An empty byte view.
    pub fn empty() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    /// Borrow the raw bytes, rejecting a NULL pointer with non-zero length and any length
    /// beyond `isize::MAX` before a slice is formed.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for reads of `len` bytes, or NULL when `len == 0`.
    pub unsafe fn as_slice(&self) -> Result<&[u8], AbiError> {
        if self.len == 0 {
            return Ok(&[]);
        }
        if self.ptr.is_null() {
            return Err(AbiError::new(
                OtelStatus::InvalidArgument,
                "bytes view has NULL ptr with non-zero len",
            ));
        }
        if self.len > isize::MAX as usize {
            return Err(AbiError::new(
                OtelStatus::InvalidArgument,
                "bytes view len exceeds the maximum supported size",
            ));
        }
        // SAFETY: non-NULL, caller guarantees `len` valid bytes, `len <= isize::MAX`.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
    }
}

/// A half-open `[first, first + count)` range into the record's borrowed node pool.
///
/// Indices are 32-bit because the per-record node budget
/// ([`OTEL_LOG_MAX_VALUE_NODES`]) is far below `u32::MAX`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OtelLogValueRange {
    /// Index of the first child node in the record's node pool.
    pub first: u32,
    /// Number of consecutive child nodes.
    pub count: u32,
}

/// Tagged-union payload for a Logs `AnyValue`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union OtelLogValuePayload {
    /// Active for [`OtelLogValueType::String`].
    pub string_value: OtelStringView,
    /// Active for [`OtelLogValueType::Bool`] (`0` = false, else true).
    pub bool_value: OtelBool,
    /// Active for [`OtelLogValueType::Int64`].
    pub int64_value: i64,
    /// Active for [`OtelLogValueType::Double`].
    pub double_value: f64,
    /// Active for [`OtelLogValueType::Bytes`].
    pub bytes_value: OtelLogBytesView,
    /// Active for [`OtelLogValueType::Array`] and [`OtelLogValueType::Map`].
    pub children: OtelLogValueRange,
}

/// A single Logs `AnyValue` (mirrors `otel_log_value_t`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogValue {
    /// Selects the active member of `value`; must be a valid [`OtelLogValueType`] tag.
    pub value_type: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// The value payload.
    pub value: OtelLogValuePayload,
}

/// A keyed Logs value (mirrors `otel_log_key_value_t`).
///
/// Used for record attributes, for map entries in the node pool (key must be non-empty and
/// unique within its map), and for array elements in the node pool (key must be empty).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogValueNode {
    /// UTF-8 key, or an empty view for array elements.
    pub key: OtelStringView,
    /// The value payload.
    pub value: OtelLogValue,
}

/// Explicit trace correlation for a log record (mirrors `otel_log_trace_context_t`).
///
/// Fixed layout, Logs-local, and independent of the C Trace API: a C caller supplies the
/// identifiers directly and never needs a live span handle.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtelLogTraceContext {
    /// W3C trace ID. Must not be all zero.
    pub trace_id: [u8; 16],
    /// W3C span ID. Must not be all zero.
    pub span_id: [u8; 8],
    /// W3C trace flags. Only [`OTEL_LOG_TRACE_FLAGS_SUPPORTED_MASK`] bits are accepted.
    pub trace_flags: u8,
    /// Reserved; every byte must be zero.
    pub reserved: [u8; 7],
}

/// The only trace flag defined by the W3C Trace Context recommendation ("sampled").
pub const OTEL_LOG_TRACE_FLAGS_SAMPLED: u8 = 0x01;
/// Mask of trace-flag bits this ABI version accepts. Any other bit is rejected.
pub const OTEL_LOG_TRACE_FLAGS_SUPPORTED_MASK: u8 = OTEL_LOG_TRACE_FLAGS_SAMPLED;

/// `present_fields` bit selecting `timestamp_unix_nanos`.
pub const OTEL_LOG_FIELD_TIMESTAMP: u64 = 1 << 0;
/// `present_fields` bit selecting `observed_timestamp_unix_nanos`.
pub const OTEL_LOG_FIELD_OBSERVED_TIMESTAMP: u64 = 1 << 1;
/// `present_fields` bit selecting `trace_context`.
pub const OTEL_LOG_FIELD_TRACE_CONTEXT: u64 = 1 << 2;
/// Every `present_fields` bit this ABI version understands. Unknown bits are rejected so a
/// newer caller cannot silently lose data against an older library.
pub const OTEL_LOG_FIELD_KNOWN_MASK: u64 =
    OTEL_LOG_FIELD_TIMESTAMP | OTEL_LOG_FIELD_OBSERVED_TIMESTAMP | OTEL_LOG_FIELD_TRACE_CONTEXT;

/// Maximum number of top-level attributes on one record.
pub const OTEL_LOG_MAX_ATTRIBUTES: usize = 256;
/// Maximum number of nodes in one record's borrowed structured-value pool. This also bounds
/// the total number of `AnyValue` nodes converted for a single record.
pub const OTEL_LOG_MAX_VALUE_NODES: usize = 1024;
/// Maximum structured-value nesting depth (a scalar body has depth 1).
pub const OTEL_LOG_MAX_VALUE_DEPTH: usize = 16;
/// Maximum length in bytes of any single string (key or value).
pub const OTEL_LOG_MAX_STRING_LEN: usize = 1 << 20;
/// Maximum length in bytes of any single byte-array value.
pub const OTEL_LOG_MAX_BYTES_LEN: usize = 1 << 20;
/// Maximum number of entries in one map value.
pub const OTEL_LOG_MAX_MAP_ENTRIES: usize = 256;
/// Maximum number of elements in one array value.
pub const OTEL_LOG_MAX_ARRAY_ELEMENTS: usize = 256;

/// Lowest valid OpenTelemetry severity number (`TRACE`).
pub const OTEL_LOG_SEVERITY_MIN: u32 = 1;
/// Highest valid OpenTelemetry severity number (`FATAL4`).
pub const OTEL_LOG_SEVERITY_MAX: u32 = 24;

/// Whether `severity` is a value this ABI version accepts.
///
/// `0` means "absent" and is accepted; `1..=24` are the canonical OpenTelemetry severity
/// numbers. Everything else is rejected.
pub const fn log_severity_is_valid(severity: u32) -> bool {
    severity <= OTEL_LOG_SEVERITY_MAX
}

/// Borrowed, one-shot view of a log record (mirrors `otel_log_record_view_t`).
///
/// Every pointer reachable from this structure is borrowed for the duration of the
/// `logger_emit` call only. An SDK-backed implementation copies everything it retains before
/// returning; no pointer into caller memory may escape the call.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogRecordView {
    /// `sizeof(otel_log_record_view_t)` as compiled by the caller. Append-only evolution.
    pub struct_size: u64,
    /// Bit set of explicitly-present optional fields ([`OTEL_LOG_FIELD_KNOWN_MASK`]).
    pub present_fields: u64,
    /// Event time in nanoseconds since the Unix epoch. Read only when
    /// [`OTEL_LOG_FIELD_TIMESTAMP`] is set (epoch zero is a valid instant, so a zero
    /// sentinel would be ambiguous).
    pub timestamp_unix_nanos: u64,
    /// Observation time in nanoseconds since the Unix epoch. Read only when
    /// [`OTEL_LOG_FIELD_OBSERVED_TIMESTAMP`] is set; otherwise the SDK supplies it.
    pub observed_timestamp_unix_nanos: u64,
    /// Canonical OpenTelemetry severity number, or `0` for absent.
    pub severity_number: u32,
    /// Reserved; must be zero.
    pub reserved_flags: u32,
    /// Record body. Use [`OtelLogValueType::Empty`] for no body.
    pub body: OtelLogValue,
    /// Attribute array; each key must be non-empty UTF-8.
    pub attributes: *const OtelLogValueNode,
    /// Number of attributes.
    pub attribute_count: usize,
    /// Borrowed node pool addressed by array/map child ranges.
    pub value_nodes: *const OtelLogValueNode,
    /// Number of nodes in the pool.
    pub value_node_count: usize,
    /// Explicit trace correlation. Read only when [`OTEL_LOG_FIELD_TRACE_CONTEXT`] is set.
    pub trace_context: OtelLogTraceContext,
    /// Reserved; every element must be zero.
    pub reserved: [u64; 4],
}

/// Instrumentation-scope configuration accepted by `provider_get_logger`.
pub type OtelLogProviderGetLogger =
    extern "C" fn(provider_ctx: *mut c_void, scope: *const OtelScopeConfig) -> *mut c_void;

/// Internal Logs implementation vtable. Logs uses a distinct ABI identifier, provider
/// context, and API-owned global slot from traces and Metrics.
///
/// Provider contexts are reference counted exactly as described for [`OtelImplVtable`]:
/// each context is one owned reference released by exactly one `provider_free`, and
/// `provider_retain` produces a new independent reference. Logger contexts are single-owner.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelLogsVtable {
    /// Logs ABI kind/version identifier.
    pub abi_version: u32,
    /// Size of this vtable in bytes. New entries may only be appended.
    pub struct_size: usize,
    /// Create a logger from a provider context and a complete instrumentation scope.
    pub provider_get_logger: OtelLogProviderGetLogger,
    /// Produce a **new owned reference** to the same provider, or NULL on failure.
    pub provider_retain: extern "C" fn(provider_ctx: *mut c_void) -> *mut c_void,
    /// Release one owned reference to a provider context.
    pub provider_free: extern "C" fn(provider_ctx: *mut c_void),
    /// Advisory enabled check for a normalized severity number.
    pub logger_enabled: extern "C" fn(logger_ctx: *mut c_void, severity: u32) -> OtelBool,
    /// Validate, copy, and emit one borrowed record. Nothing borrowed may escape the call.
    pub logger_emit:
        extern "C" fn(logger_ctx: *mut c_void, record: *const OtelLogRecordView) -> OtelStatus,
    /// Free a logger context.
    pub logger_free: extern "C" fn(logger_ctx: *mut c_void),
}

/// Current Logs implementation ABI kind/version identifier.
///
/// The high byte `0x4C` (`L`) reserves the Logs namespace; the low 24 bits carry the
/// version within that namespace.
pub const OTEL_LOGS_IMPL_ABI_VERSION: u32 = 0x4C00_0001;

/// Minimum Logs vtable prefix size required by this API version.
#[cfg(target_pointer_width = "64")]
pub const OTEL_LOGS_VTABLE_REQUIRED_SIZE: usize = 64;
#[cfg(target_pointer_width = "32")]
pub const OTEL_LOGS_VTABLE_REQUIRED_SIZE: usize = 32;

/// Validate the stable prefix of a Logs implementation vtable.
///
/// The ABI identifier selects both the Logs signal kind and its compatible version, so a
/// Trace or Metrics vtable is rejected before any function slot is read. `struct_size`
/// permits append-only extensions within the same kind.
///
/// # Safety
///
/// `vtable` must be non-NULL, correctly aligned, and readable for [`OtelVtableHeader`].
/// If the header is compatible, it must also be readable through
/// [`OTEL_LOGS_VTABLE_REQUIRED_SIZE`] and remain live wherever the caller stores or uses it.
/// No bytes beyond the header are read by this function.
pub unsafe fn logs_vtable_compatible(vtable: *const OtelLogsVtable) -> bool {
    let header = unsafe { &*vtable.cast::<OtelVtableHeader>() };
    header.abi_version == OTEL_LOGS_IMPL_ABI_VERSION
        && header.struct_size >= OTEL_LOGS_VTABLE_REQUIRED_SIZE
}

const _: () = assert!(OTEL_LOGS_IMPL_ABI_VERSION != OTEL_TRACE_IMPL_ABI_VERSION);
const _: () = assert!(OTEL_LOGS_IMPL_ABI_VERSION != OTEL_METRICS_IMPL_ABI_VERSION);
const _: () = assert!(OTEL_LOGS_IMPL_ABI_VERSION & 0xFF00_0000 == 0x4C00_0000);
const _: () = assert!(std::mem::size_of::<OtelLogsVtable>() == OTEL_LOGS_VTABLE_REQUIRED_SIZE);
const _: () = assert!(OTEL_LOG_MAX_VALUE_NODES <= u32::MAX as usize);

// Compile-time ABI guards (64-bit) mirroring the C header `_Static_assert`s.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{align_of, size_of};
    assert!(size_of::<OtelStringView>() == 16);
    assert!(align_of::<OtelStringView>() == 8);
    assert!(size_of::<OtelAttributeValue>() == 16);
    assert!(size_of::<OtelKeyValue>() == 40);
    assert!(align_of::<OtelKeyValue>() == 8);
    assert!(size_of::<OtelMetricScopeConfig>() == 64);
    assert!(align_of::<OtelMetricScopeConfig>() == 8);
    assert!(size_of::<OtelScopeConfig>() == 64);
    assert!(size_of::<OtelLogBytesView>() == 16);
    assert!(size_of::<OtelLogValueRange>() == 8);
    assert!(size_of::<OtelLogValuePayload>() == 16);
    assert!(align_of::<OtelLogValuePayload>() == 8);
    assert!(size_of::<OtelLogValue>() == 24);
    assert!(align_of::<OtelLogValue>() == 8);
    assert!(size_of::<OtelLogValueNode>() == 40);
    assert!(align_of::<OtelLogValueNode>() == 8);
    assert!(size_of::<OtelLogTraceContext>() == 32);
    assert!(size_of::<OtelLogRecordView>() == 160);
    assert!(align_of::<OtelLogRecordView>() == 8);
};

/// Assert (mostly for documentation) that a raw `void*` round-trips; used by the API/SDK.
pub fn null_mut() -> *mut c_void {
    std::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(s: &str) -> OtelStringView {
        OtelStringView {
            ptr: s.as_ptr().cast::<c_char>(),
            len: s.len(),
        }
    }

    #[test]
    fn string_helpers_roundtrip_and_guard() {
        assert_eq!(unsafe { sv("hi").as_str() }.unwrap(), "hi");
        assert_eq!(unsafe { sv("hi").to_string_strict() }.unwrap(), "hi");
        // oversized len rejected before slice creation
        let dangling = std::ptr::NonNull::<c_char>::dangling().as_ptr() as *const c_char;
        let big = OtelStringView {
            ptr: dangling,
            len: (isize::MAX as usize) + 1,
        };
        assert_eq!(
            unsafe { big.as_str() }.unwrap_err().status,
            OtelStatus::InvalidArgument
        );
    }

    #[test]
    fn lossy_matches_std() {
        let raw = b"a\xffb".as_slice();
        assert_eq!(
            try_owned_string_lossy(raw).unwrap(),
            String::from_utf8_lossy(raw)
        );
    }

    #[test]
    fn status_is_fixed_width_and_accepts_unknown_values() {
        assert_eq!(
            std::mem::size_of::<OtelStatus>(),
            std::mem::size_of::<u32>()
        );
        assert_ne!(OtelStatus(999), OtelStatus::Ok);
    }

    #[test]
    fn opaque_handle_kinds_are_globally_unique() {
        let kinds = [
            OTEL_HANDLE_KIND_TRACER_PROVIDER,
            OTEL_HANDLE_KIND_TRACER,
            OTEL_HANDLE_KIND_SPAN,
            OTEL_HANDLE_KIND_SPAN_CONTEXT,
            OTEL_HANDLE_KIND_METER_PROVIDER,
            OTEL_HANDLE_KIND_METER,
            OTEL_HANDLE_KIND_COUNTER_U64,
            OTEL_HANDLE_KIND_COUNTER_F64,
            OTEL_HANDLE_KIND_UP_DOWN_COUNTER_I64,
            OTEL_HANDLE_KIND_UP_DOWN_COUNTER_F64,
            OTEL_HANDLE_KIND_GAUGE_U64,
            OTEL_HANDLE_KIND_GAUGE_I64,
            OTEL_HANDLE_KIND_GAUGE_F64,
            OTEL_HANDLE_KIND_HISTOGRAM_U64,
            OTEL_HANDLE_KIND_HISTOGRAM_F64,
            OTEL_HANDLE_KIND_BOUND_COUNTER_U64,
            OTEL_HANDLE_KIND_BOUND_COUNTER_F64,
            OTEL_HANDLE_KIND_BOUND_HISTOGRAM_U64,
            OTEL_HANDLE_KIND_BOUND_HISTOGRAM_F64,
            OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_U64,
            OTEL_HANDLE_KIND_OBSERVABLE_COUNTER_F64,
            OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_I64,
            OTEL_HANDLE_KIND_OBSERVABLE_UP_DOWN_COUNTER_F64,
            OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_U64,
            OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_I64,
            OTEL_HANDLE_KIND_OBSERVABLE_GAUGE_F64,
            OTEL_HANDLE_KIND_SDK_BUILDER,
            OTEL_HANDLE_KIND_SDK,
            OTEL_HANDLE_KIND_OTLP_TRACE_EXPORTER_BUILDER,
            OTEL_HANDLE_KIND_TRACE_EXPORTER,
            OTEL_HANDLE_KIND_BATCH_SPAN_PROCESSOR_BUILDER,
            OTEL_HANDLE_KIND_SPAN_PROCESSOR,
            OTEL_HANDLE_KIND_OTLP_METRIC_EXPORTER_BUILDER,
            OTEL_HANDLE_KIND_METRIC_EXPORTER,
            OTEL_HANDLE_KIND_PERIODIC_METRIC_READER_BUILDER,
            OTEL_HANDLE_KIND_PERIODIC_METRIC_READER,
            OTEL_HANDLE_KIND_METRIC_VIEW_BUILDER,
            OTEL_HANDLE_KIND_METRIC_VIEW,
            OTEL_HANDLE_KIND_MANUAL_METRIC_READER,
            OTEL_HANDLE_KIND_LOGGER_PROVIDER,
            OTEL_HANDLE_KIND_LOGGER,
            OTEL_HANDLE_KIND_OTLP_LOG_EXPORTER_BUILDER,
            OTEL_HANDLE_KIND_LOG_EXPORTER,
            OTEL_HANDLE_KIND_BATCH_LOG_PROCESSOR_BUILDER,
            OTEL_HANDLE_KIND_LOG_PROCESSOR,
        ];
        for (index, kind) in kinds.iter().enumerate() {
            assert!(
                !kinds[..index].contains(kind),
                "duplicate opaque handle kind {kind:#x}"
            );
        }
    }

    #[test]
    fn discriminant_validation() {
        assert_eq!(
            OtelAttributeType::from_u32(3),
            Some(OtelAttributeType::Double)
        );
        assert_eq!(OtelAttributeType::from_u32(9), None);
        assert_eq!(OtelSpanKind::from_u32(2), Some(OtelSpanKind::Client));
        assert_eq!(OtelSpanKind::from_u32(9), None);
        assert_eq!(
            OtelSpanStatusCode::from_u32(2),
            Some(OtelSpanStatusCode::Error)
        );
        assert_eq!(OtelSpanStatusCode::from_u32(9), None);
        assert_eq!(
            OtelMetricInstrumentKind::from_u32(3),
            Some(OtelMetricInstrumentKind::Histogram)
        );
        assert_eq!(OtelMetricInstrumentKind::from_u32(99), None);
        assert_eq!(
            OtelMetricNumberKind::from_u32(2),
            Some(OtelMetricNumberKind::F64)
        );
        assert_eq!(OtelMetricNumberKind::from_u32(99), None);
        assert_eq!(OtelLogValueType::from_u32(0), Some(OtelLogValueType::Empty));
        assert_eq!(OtelLogValueType::from_u32(7), Some(OtelLogValueType::Map));
        assert_eq!(OtelLogValueType::from_u32(8), None);
        assert_eq!(OtelLogValueType::from_u32(u32::MAX), None);
    }

    #[test]
    fn log_severity_range_is_zero_through_twenty_four() {
        assert!(log_severity_is_valid(0));
        for severity in OTEL_LOG_SEVERITY_MIN..=OTEL_LOG_SEVERITY_MAX {
            assert!(log_severity_is_valid(severity));
        }
        assert!(!log_severity_is_valid(25));
        assert!(!log_severity_is_valid(u32::MAX));
    }

    #[test]
    fn logs_abi_version_is_distinct_from_other_signals() {
        assert_ne!(OTEL_LOGS_IMPL_ABI_VERSION, OTEL_TRACE_IMPL_ABI_VERSION);
        assert_ne!(OTEL_LOGS_IMPL_ABI_VERSION, OTEL_METRICS_IMPL_ABI_VERSION);
        assert_eq!(
            std::mem::size_of::<OtelLogsVtable>(),
            OTEL_LOGS_VTABLE_REQUIRED_SIZE
        );
    }

    #[test]
    fn log_presence_mask_covers_exactly_the_known_bits() {
        assert_eq!(
            OTEL_LOG_FIELD_KNOWN_MASK,
            OTEL_LOG_FIELD_TIMESTAMP
                | OTEL_LOG_FIELD_OBSERVED_TIMESTAMP
                | OTEL_LOG_FIELD_TRACE_CONTEXT
        );
        assert_eq!(OTEL_LOG_FIELD_KNOWN_MASK, 0b111);
        assert_eq!(
            OTEL_LOG_TRACE_FLAGS_SUPPORTED_MASK,
            OTEL_LOG_TRACE_FLAGS_SAMPLED
        );
    }
}
