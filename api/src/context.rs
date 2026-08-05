//! API-owned cross-signal context and authoritative per-thread attachment stack.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use opentelemetry_c_abi::{OtelHandleHeader, OTEL_HANDLE_KIND_CONTEXT};

use crate::error::{clear_last_error, fail, OtelStatus};
use crate::handle::{
    checked_ref, destroy, guard_ptr, guard_status, guard_unit, into_raw, HasHandleHeader,
};
use crate::trace::{OtelSpanContext, SpanContextData};

const MAX_CONTEXT_DEPTH: usize = 64;
const SCOPE_SIZE: usize = std::mem::size_of::<OtelContextScope>();

#[derive(Default)]
pub(crate) struct ContextData {
    pub(crate) span_context: Option<Arc<SpanContextData>>,
    pub(crate) flags: u32,
}

#[repr(C)]
pub struct OtelContext {
    header: OtelHandleHeader,
    pub(crate) data: Arc<ContextData>,
}

impl HasHandleHeader for OtelContext {
    const KIND: u64 = OTEL_HANDLE_KIND_CONTEXT;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelContextScope {
    pub struct_size: usize,
    pub thread_token: u64,
    pub generation: u64,
    pub reserved: [u64; 2],
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<OtelContextScope>() == 40);

struct StackEntry {
    generation: u64,
    context: Arc<ContextData>,
}

struct ContextStack {
    thread_token: u64,
    next_generation: u64,
    entries: Vec<StackEntry>,
}

static NEXT_THREAD_TOKEN: AtomicU64 = AtomicU64::new(1);

impl ContextStack {
    fn new() -> Self {
        let mut token = NEXT_THREAD_TOKEN.fetch_add(1, Ordering::Relaxed);
        if token == 0 {
            token = NEXT_THREAD_TOKEN.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            thread_token: token,
            next_generation: 1,
            entries: Vec::new(),
        }
    }

    fn allocate_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        generation
    }
}

thread_local! {
    static CURRENT_CONTEXT: RefCell<ContextStack> = RefCell::new(ContextStack::new());
}

fn new_handle(data: Arc<ContextData>) -> *mut OtelContext {
    into_raw(OtelContext {
        header: OtelHandleHeader::new(OtelContext::KIND),
        data,
    })
}

pub(crate) fn current_data() -> Option<Arc<ContextData>> {
    CURRENT_CONTEXT
        .try_with(|stack| {
            stack
                .borrow()
                .entries
                .last()
                .map(|e| Arc::clone(&e.context))
        })
        .ok()
        .flatten()
}

#[no_mangle]
/// Create a context from an optional live SpanContext.
///
/// # Safety
/// `span_context` must be NULL or a live handle not destroyed concurrently.
pub unsafe extern "C" fn otel_context_create(
    span_context: *const OtelSpanContext,
) -> *mut OtelContext {
    guard_ptr(|| {
        clear_last_error();
        let span_context = if span_context.is_null() {
            None
        } else {
            let Some(value) = (unsafe { checked_ref::<OtelSpanContext>(span_context) }) else {
                return std::ptr::null_mut();
            };
            if !value.is_valid() {
                fail(
                    OtelStatus::InvalidArgument,
                    "context span context is invalid",
                );
                return std::ptr::null_mut();
            }
            Some(Arc::clone(&value.data))
        };
        new_handle(Arc::new(ContextData {
            span_context,
            flags: 0,
        }))
    })
}

#[no_mangle]
/// # Safety
/// `context` must be a live handle not destroyed concurrently.
pub unsafe extern "C" fn otel_context_clone(context: *const OtelContext) -> *mut OtelContext {
    guard_ptr(|| {
        clear_last_error();
        let Some(context) = (unsafe { checked_ref::<OtelContext>(context) }) else {
            return std::ptr::null_mut();
        };
        new_handle(Arc::clone(&context.data))
    })
}

#[no_mangle]
/// # Safety
/// The returned owned handle must eventually be destroyed.
pub unsafe extern "C" fn otel_context_current() -> *mut OtelContext {
    guard_ptr(|| {
        clear_last_error();
        let data = current_data().unwrap_or_else(|| Arc::new(ContextData::default()));
        new_handle(data)
    })
}

#[no_mangle]
/// # Safety
/// `context` must be a live handle not destroyed concurrently.
pub unsafe extern "C" fn otel_context_span_context(
    context: *const OtelContext,
) -> *mut OtelSpanContext {
    guard_ptr(|| {
        clear_last_error();
        let Some(context) = (unsafe { checked_ref::<OtelContext>(context) }) else {
            return std::ptr::null_mut();
        };
        context
            .data
            .span_context
            .as_ref()
            .map_or(std::ptr::null_mut(), |data| {
                into_raw(OtelSpanContext::from_data(Arc::clone(data)))
            })
    })
}

#[no_mangle]
/// # Safety
/// `context` must be live and `scope` must point to a writable initialized token.
pub unsafe extern "C" fn otel_context_attach(
    context: *const OtelContext,
    scope: *mut OtelContextScope,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if scope.is_null() {
            return fail(OtelStatus::InvalidArgument, "context scope is NULL");
        }
        // SAFETY: caller supplies a writable token. Initialize it to an inactive state first,
        // so cleanup may call detach even when any later validation fails.
        let struct_size = unsafe { scope.cast::<usize>().read() };
        if struct_size < SCOPE_SIZE {
            return fail(
                OtelStatus::InvalidConfig,
                "context scope struct_size is too small",
            );
        }
        unsafe {
            (*scope).thread_token = 0;
            (*scope).generation = 0;
            (*scope).reserved = [0; 2];
        }
        let Some(context) = (unsafe { checked_ref::<OtelContext>(context) }) else {
            return OtelStatus::InvalidArgument;
        };
        let data = Arc::clone(&context.data);
        match CURRENT_CONTEXT.try_with(|slot| {
            let mut stack = slot.borrow_mut();
            if stack.entries.len() >= MAX_CONTEXT_DEPTH {
                return None;
            }
            let generation = stack.allocate_generation();
            let thread_token = stack.thread_token;
            stack.entries.push(StackEntry {
                generation,
                context: data,
            });
            Some((thread_token, generation))
        }) {
            Ok(Some((thread_token, generation))) => {
                unsafe {
                    (*scope).thread_token = thread_token;
                    (*scope).generation = generation;
                }
                OtelStatus::Ok
            }
            Ok(None) => fail(
                OtelStatus::InvalidConfig,
                "context attachment depth exceeds 64",
            ),
            Err(_) => fail(
                OtelStatus::InternalError,
                "thread-local context is unavailable",
            ),
        }
    })
}

#[no_mangle]
/// # Safety
/// `scope` must point to a readable/writable token initialized by this API.
pub unsafe extern "C" fn otel_context_scope_detach(scope: *mut OtelContextScope) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if scope.is_null() {
            return fail(OtelStatus::InvalidArgument, "context scope is NULL");
        }
        let struct_size = unsafe { scope.cast::<usize>().read() };
        if struct_size < SCOPE_SIZE {
            return fail(
                OtelStatus::InvalidConfig,
                "context scope struct_size is too small",
            );
        }
        let token = unsafe { &mut *scope };
        if token.reserved != [0; 2] || token.thread_token == 0 || token.generation == 0 {
            return fail(
                OtelStatus::InvalidArgument,
                "context scope is inactive or invalid",
            );
        }
        let result = CURRENT_CONTEXT.try_with(|slot| {
            let mut stack = slot.borrow_mut();
            if stack.thread_token != token.thread_token {
                return Err("context scope belongs to another thread");
            }
            if stack.entries.last().map(|e| e.generation) != Some(token.generation) {
                return Err("context scopes must be detached once in LIFO order");
            }
            stack.entries.pop();
            Ok(())
        });
        match result {
            Ok(Ok(())) => {
                token.thread_token = 0;
                token.generation = 0;
                OtelStatus::Ok
            }
            Ok(Err(message)) => fail(OtelStatus::InvalidArgument, message),
            Err(_) => fail(
                OtelStatus::InternalError,
                "thread-local context is unavailable",
            ),
        }
    })
}

#[no_mangle]
/// # Safety
/// `context` must be NULL or a live owned handle not destroyed concurrently.
pub unsafe extern "C" fn otel_context_destroy(context: *mut OtelContext) {
    guard_unit(|| unsafe { destroy(context) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{otel_span_context_create, otel_span_context_destroy};
    use opentelemetry_c_abi::OtelStringView;

    unsafe fn span_context() -> *mut OtelSpanContext {
        let trace_id = [1u8; 16];
        let span_id = [2u8; 8];
        unsafe {
            otel_span_context_create(
                trace_id.as_ptr(),
                span_id.as_ptr(),
                1,
                0,
                OtelStringView::empty(),
            )
        }
    }

    #[test]
    fn nested_scopes_restore_and_copied_tokens_fail_safely() {
        unsafe {
            let root = otel_context_create(std::ptr::null());
            let sc = span_context();
            let traced = otel_context_create(sc);
            let mut outer = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 0,
                generation: 0,
                reserved: [0; 2],
            };
            let mut inner = outer;
            assert_eq!(otel_context_attach(root, &mut outer), OtelStatus::Ok);
            let empty_snapshot = otel_context_current();
            assert!(otel_context_span_context(empty_snapshot).is_null());
            otel_context_destroy(empty_snapshot);
            assert_eq!(otel_context_attach(traced, &mut inner), OtelStatus::Ok);
            let snapshot = otel_context_current();
            let got = otel_context_span_context(snapshot);
            assert!(!got.is_null());
            let mut copied = inner;
            assert_eq!(otel_context_scope_detach(&mut copied), OtelStatus::Ok);
            assert_eq!(
                otel_context_scope_detach(&mut inner),
                OtelStatus::InvalidArgument
            );
            assert_eq!(otel_context_scope_detach(&mut outer), OtelStatus::Ok);
            otel_span_context_destroy(got);
            otel_context_destroy(snapshot);
            otel_context_destroy(root);
            otel_context_destroy(traced);
            otel_span_context_destroy(sc);
        }
    }

    #[test]
    fn out_of_order_and_failed_attach_leave_stack_usable() {
        unsafe {
            let context = otel_context_create(std::ptr::null());
            let mut first = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 0,
                generation: 0,
                reserved: [0; 2],
            };
            let mut second = first;
            assert_eq!(otel_context_attach(context, &mut first), OtelStatus::Ok);
            assert_eq!(otel_context_attach(context, &mut second), OtelStatus::Ok);
            assert_eq!(
                otel_context_scope_detach(&mut first),
                OtelStatus::InvalidArgument
            );
            assert_eq!(otel_context_scope_detach(&mut second), OtelStatus::Ok);
            assert_eq!(otel_context_scope_detach(&mut first), OtelStatus::Ok);
            let mut failed = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 99,
                generation: 99,
                reserved: [0; 2],
            };
            assert_eq!(
                otel_context_attach(std::ptr::null(), &mut failed),
                OtelStatus::InvalidArgument
            );
            assert_eq!(failed.thread_token, 0);
            assert_eq!(
                otel_context_scope_detach(&mut failed),
                OtelStatus::InvalidArgument
            );
            otel_context_destroy(context);
        }
    }

    #[test]
    fn wrong_thread_detach_fails_and_owner_can_recover() {
        unsafe {
            let context = otel_context_create(std::ptr::null());
            let mut scope = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 0,
                generation: 0,
                reserved: [0; 2],
            };
            assert_eq!(otel_context_attach(context, &mut scope), OtelStatus::Ok);
            let copied = scope;
            let result = std::thread::spawn(move || {
                let mut copied = copied;
                otel_context_scope_detach(&mut copied)
            })
            .join()
            .unwrap();
            assert_eq!(result, OtelStatus::InvalidArgument);
            assert_eq!(otel_context_scope_detach(&mut scope), OtelStatus::Ok);
            otel_context_destroy(context);
        }
    }

    #[test]
    fn depth_cap_failure_is_inactive_and_preserves_existing_stack() {
        unsafe {
            let context = otel_context_create(std::ptr::null());
            let initial = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 0,
                generation: 0,
                reserved: [0; 2],
            };
            let mut scopes = vec![initial; MAX_CONTEXT_DEPTH];
            for scope in &mut scopes {
                assert_eq!(otel_context_attach(context, scope), OtelStatus::Ok);
            }
            let mut overflow = initial;
            assert_eq!(
                otel_context_attach(context, &mut overflow),
                OtelStatus::InvalidConfig
            );
            assert_eq!(overflow.thread_token, 0);
            for scope in scopes.iter_mut().rev() {
                assert_eq!(otel_context_scope_detach(scope), OtelStatus::Ok);
            }
            otel_context_destroy(context);
        }
    }

    #[test]
    fn thread_exit_drops_undetached_stack_without_callbacks() {
        let joined = std::thread::spawn(|| unsafe {
            let context = otel_context_create(std::ptr::null());
            let mut scope = OtelContextScope {
                struct_size: SCOPE_SIZE,
                thread_token: 0,
                generation: 0,
                reserved: [0; 2],
            };
            assert_eq!(otel_context_attach(context, &mut scope), OtelStatus::Ok);
            otel_context_destroy(context);
            // Deliberately omit detach. TLS teardown owns and drops the retained Arc.
        })
        .join();
        assert!(joined.is_ok());
    }
}
