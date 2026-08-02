// SPDX-License-Identifier: Apache-2.0

//! C callback-backed Traces exporter.

use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::RwLock;
use std::time::Duration;

use opentelemetry_c_abi::OtelStatus;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::Resource;

use crate::error::{clear_last_error, fail, fail_owned};
use crate::handle::{guard_status, into_raw};
use crate::span_export_view::{convert_batch, OtelSpanExportBatchView};
use crate::trace_exporter::{OtelTraceExporter, TraceExporterImpl};

pub type OtelCustomTraceExport =
    Option<extern "C" fn(*mut c_void, *const OtelSpanExportBatchView) -> OtelStatus>;
pub type OtelCustomTraceForceFlush = Option<extern "C" fn(*mut c_void) -> OtelStatus>;
pub type OtelCustomTraceShutdown = Option<extern "C" fn(*mut c_void, u64) -> OtelStatus>;
pub type OtelCustomTraceStateDestroy = Option<extern "C" fn(*mut c_void)>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelCustomTraceExporterCallbacks {
    pub struct_size: usize,
    pub export_spans: OtelCustomTraceExport,
    pub force_flush: OtelCustomTraceForceFlush,
    pub shutdown: OtelCustomTraceShutdown,
    pub state_destroy: OtelCustomTraceStateDestroy,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<OtelCustomTraceExporterCallbacks>() == 40);
    assert!(std::mem::align_of::<OtelCustomTraceExporterCallbacks>() == 8);
    assert!(std::mem::offset_of!(OtelCustomTraceExporterCallbacks, struct_size) == 0);
    assert!(std::mem::offset_of!(OtelCustomTraceExporterCallbacks, export_spans) == 8);
    assert!(std::mem::offset_of!(OtelCustomTraceExporterCallbacks, force_flush) == 16);
    assert!(std::mem::offset_of!(OtelCustomTraceExporterCallbacks, shutdown) == 24);
    assert!(std::mem::offset_of!(OtelCustomTraceExporterCallbacks, state_destroy) == 32);
    assert!(REQUIRED_PREFIX_SIZE == 16);
};

macro_rules! member_end {
    ($field:ident, $ty:ty) => {
        std::mem::offset_of!(OtelCustomTraceExporterCallbacks, $field) + std::mem::size_of::<$ty>()
    };
}

const REQUIRED_PREFIX_SIZE: usize = member_end!(export_spans, OtelCustomTraceExport);
const FORCE_FLUSH_END: usize = member_end!(force_flush, OtelCustomTraceForceFlush);
const SHUTDOWN_END: usize = member_end!(shutdown, OtelCustomTraceShutdown);
const STATE_DESTROY_END: usize = member_end!(state_destroy, OtelCustomTraceStateDestroy);

unsafe fn read_callbacks(
    callbacks: *const OtelCustomTraceExporterCallbacks,
    struct_size: usize,
) -> OtelCustomTraceExporterCallbacks {
    let base = callbacks.cast::<u8>();
    unsafe {
        let member = |offset: usize, end: usize| {
            (struct_size >= end).then(|| base.add(offset).cast::<usize>())
        };
        OtelCustomTraceExporterCallbacks {
            struct_size,
            export_spans: std::ptr::read_unaligned(
                base.add(std::mem::offset_of!(
                    OtelCustomTraceExporterCallbacks,
                    export_spans
                ))
                .cast::<OtelCustomTraceExport>(),
            ),
            force_flush: member(
                std::mem::offset_of!(OtelCustomTraceExporterCallbacks, force_flush),
                FORCE_FLUSH_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomTraceForceFlush>())
            }),
            shutdown: member(
                std::mem::offset_of!(OtelCustomTraceExporterCallbacks, shutdown),
                SHUTDOWN_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomTraceShutdown>())
            }),
            state_destroy: member(
                std::mem::offset_of!(OtelCustomTraceExporterCallbacks, state_destroy),
                STATE_DESTROY_END,
            )
            .and_then(|pointer| {
                std::ptr::read_unaligned(pointer.cast::<OtelCustomTraceStateDestroy>())
            }),
        }
    }
}

pub(crate) struct CustomTraceExporter {
    callbacks: OtelCustomTraceExporterCallbacks,
    user_data: *mut c_void,
    resource: Resource,
    shutdown: RwLock<bool>,
}

unsafe impl Send for CustomTraceExporter {}
unsafe impl Sync for CustomTraceExporter {}

impl std::fmt::Debug for CustomTraceExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CustomTraceExporter")
    }
}

impl CustomTraceExporter {
    fn callback_result(
        &self,
        operation: &str,
        timeout: Duration,
        callback: impl FnOnce() -> OtelStatus,
    ) -> OTelSdkResult {
        let status = catch_unwind(AssertUnwindSafe(callback)).map_err(|_| {
            OTelSdkError::InternalFailure(format!(
                "custom trace exporter {operation} callback panicked"
            ))
        })?;
        match status {
            OtelStatus::Ok => Ok(()),
            OtelStatus::AlreadyShutdown => Err(OTelSdkError::AlreadyShutdown),
            OtelStatus::Timeout => Err(OTelSdkError::Timeout(timeout)),
            OtelStatus::ExportFailed | OtelStatus::InternalError => {
                Err(OTelSdkError::InternalFailure(format!(
                    "custom trace exporter {operation} callback failed with status {}",
                    status.0
                )))
            }
            status => Err(OTelSdkError::InternalFailure(format!(
                "custom trace exporter {operation} callback returned status {}, which is not a \
                 valid result for this callback",
                status.0
            ))),
        }
    }

    pub(crate) fn export(&self, spans: Vec<SpanData>) -> OTelSdkResult {
        let shutdown = self
            .shutdown
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        let Some(callback) = self.callbacks.export_spans else {
            return Err(OTelSdkError::InternalFailure(
                "custom trace exporter lost its validated export callback".to_owned(),
            ));
        };
        let storage = match convert_batch(&spans, &self.resource) {
            Ok(storage) => storage,
            Err(error) => {
                fail_owned(error.status, error.message.clone());
                return Err(OTelSdkError::InternalFailure(error.message));
            }
        };
        let result = self.callback_result("export", Duration::ZERO, || {
            callback(self.user_data, storage.view())
        });
        drop(storage);
        result
    }

    pub(crate) fn force_flush(&self) -> OTelSdkResult {
        let shutdown = self
            .shutdown
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        match self.callbacks.force_flush {
            Some(callback) => {
                self.callback_result("force-flush", Duration::ZERO, || callback(self.user_data))
            }
            None => Ok(()),
        }
    }

    pub(crate) fn shutdown(&self, timeout: Duration) -> OTelSdkResult {
        let mut shutdown = self
            .shutdown
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *shutdown {
            return Err(OTelSdkError::AlreadyShutdown);
        }
        *shutdown = true;
        match self.callbacks.shutdown {
            Some(callback) => self.callback_result("shutdown", timeout, || {
                callback(
                    self.user_data,
                    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                )
            }),
            None => Ok(()),
        }
    }

    pub(crate) fn set_resource(&mut self, resource: &Resource) {
        self.resource = resource.clone();
    }
}

impl Drop for CustomTraceExporter {
    fn drop(&mut self) {
        let _ = self.shutdown(Duration::from_secs(5));
        if let Some(destroy) = self.callbacks.state_destroy {
            let _ = catch_unwind(AssertUnwindSafe(|| destroy(self.user_data)));
        }
    }
}

/// Create a Traces exporter backed by C callbacks.
///
/// On `OTEL_STATUS_OK` the exporter owns `user_data` and invokes `state_destroy` exactly once.
/// On every failure the caller still owns `user_data` and `state_destroy` is not invoked.
///
/// # Safety
///
/// `callbacks` must address a readable callback structure whose `struct_size` describes it.
/// `out` must address writable storage. Callback state must remain valid until `state_destroy`
/// is invoked.
#[no_mangle]
pub unsafe extern "C" fn otel_custom_trace_exporter_new(
    callbacks: *const OtelCustomTraceExporterCallbacks,
    user_data: *mut c_void,
    out: *mut *mut OtelTraceExporter,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "out pointer must not be NULL");
        }
        unsafe { *out = std::ptr::null_mut() };
        if callbacks.is_null() {
            return fail(
                OtelStatus::InvalidArgument,
                "custom trace exporter callbacks must not be NULL",
            );
        }
        let struct_size = unsafe { std::ptr::read_unaligned(callbacks.cast::<usize>()) };
        if struct_size < REQUIRED_PREFIX_SIZE {
            return fail(
                OtelStatus::InvalidConfig,
                "custom trace exporter callback structure is smaller than the required ABI size",
            );
        }
        let callbacks = unsafe { read_callbacks(callbacks, struct_size) };
        if callbacks.export_spans.is_none() {
            return fail(
                OtelStatus::InvalidConfig,
                "custom trace exporter requires an export callback",
            );
        }
        let exporter = CustomTraceExporter {
            callbacks,
            user_data,
            resource: Resource::builder_empty().build(),
            shutdown: RwLock::new(false),
        };
        unsafe { *out = into_raw(OtelTraceExporter::new(TraceExporterImpl::Custom(exporter))) };
        OtelStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simple_span_processor::otel_simple_span_processor_create;
    use crate::span_processor::otel_span_processor_destroy;
    use crate::trace_exporter::otel_trace_exporter_destroy;
    use opentelemetry::trace::{Span, Tracer, TracerProvider};
    use opentelemetry::{Array, KeyValue, Value};
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct State {
        exports: AtomicUsize,
        flushes: AtomicUsize,
        shutdowns: AtomicUsize,
        destroys: AtomicUsize,
        records: AtomicUsize,
        attrs: AtomicUsize,
        events: AtomicUsize,
        links: AtomicUsize,
        resource_attrs: AtomicUsize,
    }

    impl State {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                exports: AtomicUsize::new(0),
                flushes: AtomicUsize::new(0),
                shutdowns: AtomicUsize::new(0),
                destroys: AtomicUsize::new(0),
                records: AtomicUsize::new(0),
                attrs: AtomicUsize::new(0),
                events: AtomicUsize::new(0),
                links: AtomicUsize::new(0),
                resource_attrs: AtomicUsize::new(0),
            })
        }
    }

    extern "C" fn export(
        user_data: *mut c_void,
        batch: *const OtelSpanExportBatchView,
    ) -> OtelStatus {
        let state = unsafe { &*(user_data.cast::<State>()) };
        state.exports.fetch_add(1, Ordering::SeqCst);
        let batch = unsafe { &*batch };
        state
            .records
            .fetch_add(batch.record_count, Ordering::SeqCst);
        state
            .resource_attrs
            .fetch_add(batch.resource_attribute_count, Ordering::SeqCst);
        if batch.record_count > 0 {
            let record = unsafe { &*batch.records };
            state
                .attrs
                .fetch_add(record.attribute_count, Ordering::SeqCst);
            state.events.fetch_add(record.event_count, Ordering::SeqCst);
            state.links.fetch_add(record.link_count, Ordering::SeqCst);
            assert_eq!(record.status_code, 2);
            assert!(!record.scope.is_null());
            if record.attribute_count > 0 {
                let attr = unsafe { &*record.attributes };
                if attr.value_type == crate::span_export_view::OTEL_SPAN_ATTRIBUTE_INT64_ARRAY {
                    assert_eq!(unsafe { attr.value.array.count }, 3);
                }
            }
        }
        OtelStatus::Ok
    }

    extern "C" fn force_flush(user_data: *mut c_void) -> OtelStatus {
        unsafe { &*(user_data.cast::<State>()) }
            .flushes
            .fetch_add(1, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn shutdown(user_data: *mut c_void, _timeout_millis: u64) -> OtelStatus {
        unsafe { &*(user_data.cast::<State>()) }
            .shutdowns
            .fetch_add(1, Ordering::SeqCst);
        OtelStatus::Ok
    }

    extern "C" fn destroy(user_data: *mut c_void) {
        let state = unsafe { Arc::from_raw(user_data.cast::<State>()) };
        state.destroys.fetch_add(1, Ordering::SeqCst);
    }

    fn callbacks() -> OtelCustomTraceExporterCallbacks {
        OtelCustomTraceExporterCallbacks {
            struct_size: std::mem::size_of::<OtelCustomTraceExporterCallbacks>(),
            export_spans: Some(export),
            force_flush: Some(force_flush),
            shutdown: Some(shutdown),
            state_destroy: Some(destroy),
        }
    }

    fn truncated_table(prefix_size: usize) -> Box<[usize]> {
        let word = std::mem::size_of::<usize>();
        assert_eq!(prefix_size % word, 0);
        let full = callbacks();
        let words = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&full).cast::<usize>(),
                std::mem::size_of::<OtelCustomTraceExporterCallbacks>() / word,
            )
        };
        let mut table = words[..prefix_size / word].to_vec();
        table[0] = prefix_size;
        table.into_boxed_slice()
    }

    #[test]
    fn construction_validates_callback_table() {
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(
                    &callbacks(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            OtelStatus::InvalidArgument
        );
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut exporter,
                )
            },
            OtelStatus::InvalidArgument
        );
        let mut short = callbacks();
        short.struct_size = 0;
        assert_eq!(
            unsafe { otel_custom_trace_exporter_new(&short, std::ptr::null_mut(), &mut exporter) },
            OtelStatus::InvalidConfig
        );
        let mut missing = callbacks();
        missing.export_spans = None;
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(&missing, std::ptr::null_mut(), &mut exporter)
            },
            OtelStatus::InvalidConfig
        );
    }

    #[test]
    fn required_prefix_omits_optional_callbacks() {
        let table = truncated_table(REQUIRED_PREFIX_SIZE);
        let parsed = unsafe { read_callbacks(table.as_ptr().cast(), REQUIRED_PREFIX_SIZE) };
        assert!(parsed.export_spans.is_some());
        assert!(parsed.force_flush.is_none());
        assert!(parsed.shutdown.is_none());
        assert!(parsed.state_destroy.is_none());
    }

    #[test]
    fn owns_state_only_after_success() {
        let state = State::new();
        let raw = Arc::into_raw(Arc::clone(&state));
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(&callbacks(), raw.cast_mut().cast(), &mut exporter)
            },
            OtelStatus::Ok
        );
        unsafe { otel_trace_exporter_destroy(exporter) };
        assert_eq!(state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(state.destroys.load(Ordering::SeqCst), 1);

        let raw = Arc::into_raw(Arc::clone(&state));
        let mut invalid = callbacks();
        invalid.struct_size = 0;
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(&invalid, raw.cast_mut().cast(), &mut exporter)
            },
            OtelStatus::InvalidConfig
        );
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn simple_processor_exports_semantic_span_batch() {
        let state = State::new();
        let mut exporter = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                otel_custom_trace_exporter_new(
                    &callbacks(),
                    Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
                    &mut exporter,
                )
            },
            OtelStatus::Ok
        );
        let mut processor = std::ptr::null_mut();
        assert_eq!(
            unsafe { otel_simple_span_processor_create(exporter, &mut processor) },
            OtelStatus::Ok
        );
        unsafe { otel_span_processor_destroy(processor) };

        let exporter = CustomTraceExporter {
            callbacks: callbacks(),
            user_data: Arc::into_raw(Arc::clone(&state)).cast_mut().cast(),
            resource: Resource::builder_empty()
                .with_attributes([KeyValue::new("service.name", "svc")])
                .build(),
            shutdown: RwLock::new(false),
        };
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(TraceExporterImpl::Custom(exporter))
            .with_resource(
                Resource::builder_empty()
                    .with_attributes([KeyValue::new("service.name", "svc")])
                    .build(),
            )
            .build();
        let tracer = provider.tracer("custom-trace-test");
        let mut span = tracer.start("custom-span");
        span.set_attribute(KeyValue::new(
            "array",
            Value::Array(Array::I64(vec![1, 2, 3])),
        ));
        span.add_event("event", vec![KeyValue::new("event.attr", true)]);
        span.add_link(
            span.span_context().clone(),
            vec![KeyValue::new("link.attr", "value")],
        );
        span.set_status(opentelemetry::trace::Status::error("failed"));
        span.end();
        provider.force_flush().unwrap();
        provider.shutdown().unwrap();
        assert!(state.exports.load(Ordering::SeqCst) >= 1);
        assert_eq!(state.records.load(Ordering::SeqCst), 1);
        assert_eq!(state.attrs.load(Ordering::SeqCst), 1);
        assert_eq!(state.events.load(Ordering::SeqCst), 1);
        assert_eq!(state.links.load(Ordering::SeqCst), 1);
        assert!(state.resource_attrs.load(Ordering::SeqCst) >= 1);
    }
}
