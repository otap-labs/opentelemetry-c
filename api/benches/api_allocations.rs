// SPDX-License-Identifier: Apache-2.0

//! Enforced steady-state allocation contract for API-only trace recording.
//!
//! Setup is deliberately outside the measured regions. With no SDK installed, starting,
//! using, ending, and destroying a no-op span must not touch the heap.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use opentelemetry_c_api::{
    otel_global_tracer_provider, otel_span_add_event, otel_span_destroy, otel_span_end,
    otel_span_set_bool_attribute, otel_span_set_string_attribute, otel_tracer_destroy,
    otel_tracer_provider_destroy, otel_tracer_provider_get_tracer, otel_tracer_start_span,
    OtelAttributeType, OtelAttributeValue, OtelKeyValue, OtelSpan, OtelStatus, OtelStringView,
};

const ITERATIONS: u64 = 10_000;
const WARMUP_ITERATIONS: u64 = 1_000;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast::<c_char>(),
        len: value.len(),
    }
}

fn empty() -> OtelStringView {
    OtelStringView {
        ptr: ptr::null(),
        len: 0,
    }
}

fn measure_zero(name: &str, mut operation: impl FnMut()) {
    for _ in 0..WARMUP_ITERATIONS {
        operation();
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::SeqCst);
    for _ in 0..ITERATIONS {
        operation();
    }
    COUNTING.store(false, Ordering::SeqCst);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    println!("{name},{ITERATIONS},{allocations}");
    assert_eq!(
        allocations, 0,
        "API-only steady-state trace operation allocated in {name}"
    );
}

fn main() {
    println!("benchmark,iterations,total_allocations");

    let provider = otel_global_tracer_provider();
    let tracer =
        unsafe { otel_tracer_provider_get_tracer(provider, sv("alloc"), empty(), empty()) };
    unsafe { otel_tracer_provider_destroy(provider) };
    assert!(!tracer.is_null());

    measure_zero("api_no_sdk/start_end_destroy", || {
        let span = unsafe { otel_tracer_start_span(tracer, sv("operation"), ptr::null()) };
        assert!(!span.is_null());
        assert_eq!(unsafe { otel_span_end(span) }, OtelStatus::Ok);
        unsafe { otel_span_destroy(span) };
    });

    let span: *mut OtelSpan = unsafe { otel_tracer_start_span(tracer, sv("cached"), ptr::null()) };
    assert!(!span.is_null());
    let event_attributes = [OtelKeyValue {
        key: sv("attempt"),
        value_type: OtelAttributeType::Int64 as u32,
        value: OtelAttributeValue { int64_value: 1 },
    }];
    measure_zero("api_no_sdk/span_attributes", || {
        assert_eq!(
            unsafe { otel_span_set_string_attribute(span, sv("http.method"), sv("GET")) },
            OtelStatus::Ok
        );
        assert_eq!(
            unsafe { otel_span_set_bool_attribute(span, sv("cache.hit"), 1) },
            OtelStatus::Ok
        );
    });
    measure_zero("api_no_sdk/span_event", || {
        assert_eq!(
            unsafe {
                otel_span_add_event(
                    span,
                    sv("retry"),
                    event_attributes.as_ptr(),
                    event_attributes.len(),
                )
            },
            OtelStatus::Ok
        );
    });
    black_box(span);
    unsafe {
        otel_span_destroy(span);
        otel_tracer_destroy(tracer);
    }
}
