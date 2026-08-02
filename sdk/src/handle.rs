// SPDX-License-Identifier: Apache-2.0

//! Handle plumbing for the SDK crate's own handles (`otel_sdk_builder_t`, `otel_sdk_t`).
//!
//! Mirrors the API crate's handle plumbing, but diagnostics are recorded in the API-owned
//! error slot via [`crate::api_ffi`] so `otel_last_error_message()` (exported by the API)
//! returns them.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use opentelemetry_c_abi::{OtelHandleHeader, OtelStatus};

use crate::api_ffi;

pub(crate) trait HasHandleHeader {
    const KIND: u64;
    fn header(&self) -> &OtelHandleHeader;
    fn header_mut(&mut self) -> &mut OtelHandleHeader;
}

unsafe fn read_header<T>(ptr: *const T) -> OtelHandleHeader {
    // SAFETY: callers guarantee NULL or a live project handle. All project handles have the
    // same aligned `#[repr(C)]` prefix, including live handles of another kind.
    unsafe { ptr::read(ptr.cast::<OtelHandleHeader>()) }
}

/// # Safety
/// `ptr` must be NULL or a live handle of the exact type `T`, not destroyed concurrently.
pub(crate) unsafe fn checked_ref<'a, T: HasHandleHeader>(ptr: *const T) -> Option<&'a T> {
    if ptr.is_null() {
        api_ffi::set_last_error("null handle passed to OpenTelemetry C API");
        return None;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live() {
        api_ffi::set_last_error("handle failed validation: not a live OpenTelemetry C handle");
        return None;
    }
    if header.kind() != T::KIND {
        api_ffi::set_last_error("handle failed validation: wrong OpenTelemetry C handle type");
        return None;
    }
    let handle = unsafe { &*ptr };
    debug_assert!(handle.header().is_live_kind(T::KIND));
    Some(handle)
}

/// # Safety
/// `ptr` must be NULL or a live, uniquely-borrowed handle of the exact type `T`.
pub(crate) unsafe fn checked_mut<'a, T: HasHandleHeader>(ptr: *mut T) -> Option<&'a mut T> {
    if ptr.is_null() {
        api_ffi::set_last_error("null handle passed to OpenTelemetry C API");
        return None;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live() {
        api_ffi::set_last_error("handle failed validation: not a live OpenTelemetry C handle");
        return None;
    }
    if header.kind() != T::KIND {
        api_ffi::set_last_error("handle failed validation: wrong OpenTelemetry C handle type");
        return None;
    }
    let handle = unsafe { &mut *ptr };
    debug_assert!(handle.header().is_live_kind(T::KIND));
    Some(handle)
}

pub(crate) fn into_raw<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// # Safety
/// `ptr` must be NULL or a pointer from [`into_raw`] for the same `T`, not double-freed.
pub(crate) unsafe fn destroy<T: HasHandleHeader>(ptr: *mut T) {
    if ptr.is_null() {
        return;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live_kind(T::KIND) {
        return;
    }
    let handle = unsafe { &mut *ptr };
    handle.header_mut().poison();
    drop(unsafe { Box::from_raw(ptr) });
}

/// Take ownership of a handle for an **ownership transfer** (e.g. moving an exporter into a
/// processor builder). Validates the handle, poisons its magic, and returns the owned `Box`.
/// Once `Some` is returned, the original pointer is consumed and must never be accessed again;
/// the returned box may be moved out of and deallocated immediately. Returns `None` (with the
/// last-error set) for a NULL/wrong/dead handle — in which case nothing is consumed and the
/// caller still owns the original handle.
///
/// # Safety
/// `ptr` must be NULL or a live handle of the exact type `T` from [`into_raw`], not used
/// concurrently.
pub(crate) unsafe fn take<T: HasHandleHeader>(ptr: *mut T) -> Option<Box<T>> {
    if ptr.is_null() {
        api_ffi::set_last_error("null handle passed to OpenTelemetry C API");
        return None;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live() {
        api_ffi::set_last_error("handle failed validation: not a live OpenTelemetry C handle");
        return None;
    }
    if header.kind() != T::KIND {
        api_ffi::set_last_error("handle failed validation: wrong OpenTelemetry C handle type");
        return None;
    }
    let handle = unsafe { &mut *ptr };
    // Reject accidental use while the handle allocation remains alive inside a new owner.
    handle.header_mut().poison();
    Some(unsafe { Box::from_raw(ptr) })
}

pub(crate) fn guard_status<F: FnOnce() -> OtelStatus>(f: F) -> OtelStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(s) => s,
        Err(_) => {
            api_ffi::set_last_error("caught panic at FFI boundary");
            OtelStatus::InternalError
        }
    }
}

pub(crate) fn guard_ptr<T, F: FnOnce() -> *mut T>(f: F) -> *mut T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(p) => p,
        Err(_) => {
            api_ffi::set_last_error("caught panic at FFI boundary");
            std::ptr::null_mut()
        }
    }
}

pub(crate) fn guard_unit<F: FnOnce()>(f: F) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct Dummy {
        header: OtelHandleHeader,
        value: u64,
    }

    impl HasHandleHeader for Dummy {
        const KIND: u64 = 0xFFFF_1001;
        fn header(&self) -> &OtelHandleHeader {
            &self.header
        }
        fn header_mut(&mut self) -> &mut OtelHandleHeader {
            &mut self.header
        }
    }

    #[repr(C)]
    struct Other {
        header: OtelHandleHeader,
    }

    impl HasHandleHeader for Other {
        const KIND: u64 = 0xFFFF_1002;
        fn header(&self) -> &OtelHandleHeader {
            &self.header
        }
        fn header_mut(&mut self) -> &mut OtelHandleHeader {
            &mut self.header
        }
    }

    #[test]
    fn prefix_validation_rejects_live_wrong_type_before_typed_access() {
        let other = into_raw(Other {
            header: OtelHandleHeader::new(Other::KIND),
        });
        assert!(unsafe { checked_ref::<Dummy>(other.cast()) }.is_none());
        assert!(unsafe { checked_mut::<Dummy>(other.cast()) }.is_none());
        assert!(unsafe { take::<Dummy>(other.cast()) }.is_none());
        unsafe { destroy(other) };

        let dummy = into_raw(Dummy {
            header: OtelHandleHeader::new(Dummy::KIND),
            value: 7,
        });
        assert_eq!(unsafe { checked_ref(dummy) }.unwrap().value, 7);
        unsafe { destroy(dummy) };
    }
}
