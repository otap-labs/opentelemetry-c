#![no_main]

use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use opentelemetry_c_api::{
    otel_counter_u64_add, otel_counter_u64_destroy, otel_meter_create_u64_counter,
    otel_meter_destroy, otel_meter_provider_destroy, otel_meter_provider_get_meter, OtelCounterU64,
    OtelMeterProvider, OtelStatus, OtelStringView,
};
use opentelemetry_c_sdk::{
    otel_custom_metric_exporter_new, otel_manual_metric_reader_destroy,
    otel_manual_metric_reader_new, otel_metric_batch_visit, otel_metric_exporter_destroy,
    otel_sdk_build, otel_sdk_builder_add_manual_metric_reader, otel_sdk_builder_destroy,
    otel_sdk_builder_new, otel_sdk_destroy, otel_sdk_metrics_force_flush,
    otel_sdk_metrics_shutdown, OtelCustomMetricExporterCallbacks, OtelManualMetricReader,
    OtelMetricBatch, OtelMetricExporter, OtelMetricMetadata, OtelMetricVisitor, OtelSdk,
};

#[derive(Arbitrary, Debug)]
struct Input {
    callback_size: usize,
    visitor_size: usize,
    temporality: u32,
    callback_status: u32,
    include_export: bool,
    include_metric_visitor: bool,
    value: u64,
}

struct State {
    visitor_size: usize,
    callback_status: u32,
    include_metric_visitor: bool,
    exports: AtomicUsize,
    destroys: AtomicUsize,
}

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

extern "C" fn visit_metric(data: *mut c_void, _: *const OtelMetricMetadata) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
    OtelStatus(state.callback_status)
}

extern "C" fn export_metrics(data: *mut c_void, batch: *const OtelMetricBatch) -> OtelStatus {
    let state = unsafe { &*(data.cast::<State>()) };
    state.exports.fetch_add(1, Ordering::Relaxed);
    let visitor = OtelMetricVisitor {
        struct_size: state.visitor_size,
        resource: None,
        scope: None,
        metric: state.include_metric_visitor.then_some(visit_metric),
        point: None,
        exemplar: None,
    };
    unsafe { otel_metric_batch_visit(batch, &visitor, data) }
}

extern "C" fn destroy_state(data: *mut c_void) {
    let state = unsafe { &*(data.cast::<State>()) };
    state.destroys.fetch_add(1, Ordering::Relaxed);
}

fn prefix_size(raw: usize, complete: usize) -> usize {
    match raw % 5 {
        0 => complete,
        1 => 0,
        2 => complete.saturating_sub(1),
        3 => complete.saturating_add(1),
        _ => raw,
    }
}

fuzz_target!(|input: Input| {
    let state = State {
        visitor_size: prefix_size(input.visitor_size, std::mem::size_of::<OtelMetricVisitor>()),
        callback_status: match input.callback_status % 10 {
            0..=7 => input.callback_status % 10,
            _ => input.callback_status,
        },
        include_metric_visitor: input.include_metric_visitor,
        exports: AtomicUsize::new(0),
        destroys: AtomicUsize::new(0),
    };
    let callbacks = OtelCustomMetricExporterCallbacks {
        struct_size: prefix_size(
            input.callback_size,
            std::mem::size_of::<OtelCustomMetricExporterCallbacks>(),
        ),
        export_metrics: input.include_export.then_some(export_metrics),
        force_flush: None,
        shutdown: None,
        state_destroy: Some(destroy_state),
    };
    let mut exporter: *mut OtelMetricExporter = ptr::null_mut();
    let exporter_status = unsafe {
        otel_custom_metric_exporter_new(
            &callbacks,
            (&state as *const State).cast_mut().cast(),
            match input.temporality % 6 {
                0..=3 => input.temporality % 6,
                _ => input.temporality,
            },
            &mut exporter,
        )
    };
    if exporter_status != OtelStatus::Ok {
        assert!(exporter.is_null());
        assert_eq!(state.destroys.load(Ordering::Relaxed), 0);
        return;
    }

    let mut reader: *mut OtelManualMetricReader = ptr::null_mut();
    if unsafe { otel_manual_metric_reader_new(exporter, &mut reader) } != OtelStatus::Ok {
        unsafe { otel_metric_exporter_destroy(exporter) };
        return;
    }
    let builder = otel_sdk_builder_new();
    if unsafe { otel_sdk_builder_add_manual_metric_reader(builder, reader) } != OtelStatus::Ok {
        unsafe {
            otel_manual_metric_reader_destroy(reader);
            otel_sdk_builder_destroy(builder);
        };
        return;
    }
    let mut sdk: *mut OtelSdk = ptr::null_mut();
    if unsafe { otel_sdk_build(builder, &mut sdk) } != OtelStatus::Ok {
        unsafe { otel_sdk_builder_destroy(builder) };
        return;
    }
    unsafe { otel_sdk_builder_destroy(builder) };

    let provider = unsafe { opentelemetry_c_sdk::otel_sdk_get_meter_provider(sdk) }
        .cast::<OtelMeterProvider>();
    let meter = unsafe { otel_meter_provider_get_meter(provider, sv("fuzz"), sv(""), sv("")) };
    let mut counter: *mut OtelCounterU64 = ptr::null_mut();
    if !meter.is_null()
        && unsafe {
            otel_meter_create_u64_counter(meter, sv("requests"), ptr::null(), &mut counter)
        } == OtelStatus::Ok
    {
        let _ = unsafe { otel_counter_u64_add(counter, input.value, ptr::null(), 0) };
        let _ = unsafe { otel_sdk_metrics_force_flush(sdk, 0) };
    }

    unsafe {
        otel_counter_u64_destroy(counter);
        otel_meter_destroy(meter);
        otel_meter_provider_destroy(provider);
        let _ = otel_sdk_metrics_shutdown(sdk, 1_000);
        otel_sdk_destroy(sdk);
    }
    assert_eq!(state.destroys.load(Ordering::Relaxed), 1);
});
