#![no_main]

use libfuzzer_sys::fuzz_target;
use opentelemetry_c_abi::{OtelStatus, OtelStringView};
use opentelemetry_c_api::{
    otel_context_attach, otel_context_create, otel_context_current, otel_context_destroy,
    otel_context_scope_detach, otel_context_span_context, otel_span_context_create,
    otel_span_context_destroy, OtelContextScope,
};

fuzz_target!(|data: &[u8]| {
    unsafe {
        let trace_id = [1u8; 16];
        let span_id = [2u8; 8];
        let span_context = otel_span_context_create(
            trace_id.as_ptr(), span_id.as_ptr(), 1, 0, OtelStringView::empty(),
        );
        if span_context.is_null() { return; }
        let empty = otel_context_create(std::ptr::null());
        let traced = otel_context_create(span_context);
        if empty.is_null() || traced.is_null() {
            otel_context_destroy(empty); otel_context_destroy(traced);
            otel_span_context_destroy(span_context); return;
        }
        let mut scopes: Vec<OtelContextScope> = Vec::new();
        let mut stale: Option<OtelContextScope> = None;
        for &operation in data.iter().take(256) {
            match operation % 5 {
                0 | 1 => {
                    let mut scope = OtelContextScope {
                        struct_size: std::mem::size_of::<OtelContextScope>(),
                        thread_token: 0, generation: 0, reserved: [0; 2],
                    };
                    if otel_context_attach(if operation & 1 == 0 { empty } else { traced }, &mut scope) == OtelStatus::Ok {
                        stale = Some(scope);
                        scopes.push(scope);
                    }
                }
                2 => {
                    if let Some(mut scope) = scopes.pop() { let _ = otel_context_scope_detach(&mut scope); }
                }
                3 => {
                    if let Some(mut copy) = stale { let _ = otel_context_scope_detach(&mut copy); }
                }
                _ => {
                    let snapshot = otel_context_current();
                    if !snapshot.is_null() {
                        let sc = otel_context_span_context(snapshot);
                        otel_span_context_destroy(sc);
                        otel_context_destroy(snapshot);
                    }
                }
            }
        }
        while let Some(mut scope) = scopes.pop() { let _ = otel_context_scope_detach(&mut scope); }
        otel_context_destroy(empty);
        otel_context_destroy(traced);
        otel_span_context_destroy(span_context);
    }
});
