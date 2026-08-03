// SPDX-License-Identifier: Apache-2.0

//! Opaque handle plumbing: raw-prefix validation, allocation helpers, and
//! panic-catching wrappers used by every FFI entry point in the API crate.
//!
//! ## What the raw prefix can and cannot do
//!
//! The common `#[repr(C)]` prefix safely distinguishes live handles created by this project
//! before a full typed reference is formed. It remains a **defensive diagnostic, not a
//! memory-safety boundary**: arbitrary/foreign pointers, freed pointers, double destruction,
//! or racing destruction remain undefined behavior.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::error::{set_last_error, OtelStatus};
use opentelemetry_c_abi::OtelHandleHeader;

/// A heap-allocated handle with [`OtelHandleHeader`] as its first `#[repr(C)]` field.
pub(crate) trait HasHandleHeader {
    /// Globally unique handle kind from the internal ABI crate.
    const KIND: u64;
    fn header(&self) -> &OtelHandleHeader;
    fn header_mut(&mut self) -> &mut OtelHandleHeader;
}

unsafe fn read_header<T>(ptr: *const T) -> OtelHandleHeader {
    // SAFETY: callers guarantee NULL or a live project handle. Every project handle starts
    // with the same aligned `#[repr(C)]` header, including live handles of another kind.
    unsafe { ptr::read(ptr.cast::<OtelHandleHeader>()) }
}

/// Borrow a `&T` after validating only the common raw prefix.
///
/// # Safety
/// `ptr` must be NULL or point to a live project handle, not destroyed or mutated
/// concurrently for the borrow. A live handle of another project handle kind is rejected.
pub(crate) unsafe fn checked_ref<'a, T: HasHandleHeader>(ptr: *const T) -> Option<&'a T> {
    if ptr.is_null() {
        set_last_error("null handle passed to OpenTelemetry C API");
        return None;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live() {
        set_last_error("handle failed validation: not a live OpenTelemetry C handle");
        return None;
    }
    if header.kind() != T::KIND {
        set_last_error("handle failed validation: wrong OpenTelemetry C handle type");
        return None;
    }
    // SAFETY: the common header established that this is a live handle of kind `T`.
    let handle = unsafe { &*ptr };
    debug_assert!(handle.header().is_live_kind(T::KIND));
    Some(handle)
}

/// Allocate a handle on the heap and return an owning raw pointer for C.
pub(crate) fn into_raw<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// Reclaim and drop a handle previously created with [`into_raw`].
///
/// NULL is ignored; a magic mismatch leaves the allocation untouched. Best-effort only:
/// use-after-destroy, double-destroy, or racing `destroy` is undefined behavior.
///
/// # Safety
/// `ptr` must be NULL or a pointer returned by [`into_raw`] for the same `T` that has not
/// been destroyed, and must not be used or destroyed concurrently.
pub(crate) unsafe fn destroy<T: HasHandleHeader>(ptr: *mut T) {
    if ptr.is_null() {
        return;
    }
    let header = unsafe { read_header(ptr) };
    if !header.is_live_kind(T::KIND) {
        return;
    }
    // SAFETY: the raw header established the concrete handle kind.
    let handle = unsafe { &mut *ptr };
    handle.header_mut().poison();
    // SAFETY: validated above; reconstitute the Box to run drop + free.
    drop(unsafe { Box::from_raw(ptr) });
}

/// Run `f`, converting any Rust panic into [`OtelStatus::InternalError`].
pub(crate) fn guard_status<F: FnOnce() -> OtelStatus>(f: F) -> OtelStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(_) => {
            set_last_error("caught panic at FFI boundary");
            OtelStatus::InternalError
        }
    }
}

/// Run `f`, converting any Rust panic into a NULL pointer.
pub(crate) fn guard_ptr<T, F: FnOnce() -> *mut T>(f: F) -> *mut T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(ptr) => ptr,
        Err(_) => {
            set_last_error("caught panic at FFI boundary");
            std::ptr::null_mut()
        }
    }
}

/// Run `f`, swallowing any Rust panic (for `void` destructors/setters).
pub(crate) fn guard_unit<F: FnOnce()>(f: F) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

/// Run `f`, returning `fallback` if it panics (for plain-value getters).
pub(crate) fn guard_value<T, F: FnOnce() -> T>(fallback: T, f: F) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct Dummy {
        header: OtelHandleHeader,
        value: i32,
    }
    impl HasHandleHeader for Dummy {
        const KIND: u64 = 0xFFFF_0001;
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
        const KIND: u64 = 0xFFFF_0002;
        fn header(&self) -> &OtelHandleHeader {
            &self.header
        }
        fn header_mut(&mut self) -> &mut OtelHandleHeader {
            &mut self.header
        }
    }

    #[test]
    fn ref_rejects_null_dead_and_live_wrong_type_then_roundtrips() {
        assert!(unsafe { checked_ref::<Dummy>(std::ptr::null()) }.is_none());
        let bad = into_raw(Dummy {
            header: OtelHandleHeader::new(Dummy::KIND),
            value: 1,
        });
        unsafe { (*bad).header.poison() };
        assert!(unsafe { checked_ref(bad) }.is_none());
        unsafe { drop(Box::from_raw(bad)) };

        let other = into_raw(Other {
            header: OtelHandleHeader::new(Other::KIND),
        });
        assert!(unsafe { checked_ref::<Dummy>(other.cast()) }.is_none());
        unsafe { destroy(other) };

        let ptr = into_raw(Dummy {
            header: OtelHandleHeader::new(Dummy::KIND),
            value: 7,
        });
        assert_eq!(unsafe { checked_ref(ptr) }.unwrap().value, 7);
        unsafe { destroy(ptr) };
    }

    #[test]
    fn guards_catch_panics() {
        assert_eq!(guard_status(|| panic!("x")), OtelStatus::InternalError);
        assert!(guard_ptr::<u8, _>(|| panic!("x")).is_null());
        guard_unit(|| panic!("x"));
        assert_eq!(guard_value(5, || panic!("x")), 5);
    }
}
