//! API-owned immutable baggage and bounded W3C baggage propagation.

use std::ffi::c_void;
use std::sync::Arc;

use opentelemetry_c_abi::{
    OtelHandleHeader, OtelStatus, OtelStringView, OTEL_HANDLE_KIND_BAGGAGE,
    OTEL_HANDLE_KIND_BAGGAGE_BUILDER,
};

use crate::error::{clear_last_error, fail};
use crate::handle::{
    checked_mut, checked_ref, destroy, guard_ptr, guard_status, guard_unit, guard_value, into_raw,
    HasHandleHeader,
};

pub(crate) const MAX_BAGGAGE_ITEMS: usize = 64;
pub(crate) const MAX_BAGGAGE_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BaggageEntry {
    key: String,
    value: String,
    metadata: String,
}

#[derive(Default, Debug)]
pub(crate) struct BaggageData {
    entries: Vec<BaggageEntry>,
}

#[repr(C)]
pub struct OtelBaggage {
    header: OtelHandleHeader,
    pub(crate) data: Arc<BaggageData>,
}

impl HasHandleHeader for OtelBaggage {
    const KIND: u64 = OTEL_HANDLE_KIND_BAGGAGE;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

#[repr(C)]
pub struct OtelBaggageBuilder {
    header: OtelHandleHeader,
    entries: Vec<BaggageEntry>,
}

impl HasHandleHeader for OtelBaggageBuilder {
    const KIND: u64 = OTEL_HANDLE_KIND_BAGGAGE_BUILDER;
    fn header(&self) -> &OtelHandleHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut OtelHandleHeader {
        &mut self.header
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OtelBaggageEntryView {
    pub struct_size: usize,
    pub key: OtelStringView,
    pub value: OtelStringView,
    pub metadata: OtelStringView,
}

const BAGGAGE_ENTRY_VIEW_V1_SIZE: usize = std::mem::size_of::<OtelBaggageEntryView>();
#[cfg(target_pointer_width = "64")]
const _: () = assert!(BAGGAGE_ENTRY_VIEW_V1_SIZE == 56);

pub type OtelBaggageVisitor =
    Option<extern "C" fn(*mut c_void, *const OtelBaggageEntryView) -> OtelStatus>;

fn sv(value: &str) -> OtelStringView {
    OtelStringView {
        ptr: value.as_ptr().cast(),
        len: value.len(),
    }
}

fn view(entry: &BaggageEntry) -> OtelBaggageEntryView {
    OtelBaggageEntryView {
        struct_size: std::mem::size_of::<OtelBaggageEntryView>(),
        key: sv(&entry.key),
        value: sv(&entry.value),
        metadata: sv(&entry.metadata),
    }
}

unsafe fn string_from_view(
    value: OtelStringView,
    field: &'static str,
) -> Result<String, OtelStatus> {
    if value.len != 0 && value.ptr.is_null() {
        return Err(fail(OtelStatus::InvalidArgument, field));
    }
    let bytes = if value.len == 0 {
        &[][..]
    } else {
        // SAFETY: the caller promises a readable view for the duration of the call.
        unsafe { std::slice::from_raw_parts(value.ptr.cast::<u8>(), value.len) }
    };
    let text = std::str::from_utf8(bytes).map_err(|_| fail(OtelStatus::InvalidUtf8, field))?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| fail(OtelStatus::InternalError, "baggage allocation failed"))?;
    owned.push_str(text);
    Ok(owned)
}

unsafe fn str_from_view<'a>(
    value: OtelStringView,
    field: &'static str,
) -> Result<&'a str, OtelStatus> {
    if value.len != 0 && value.ptr.is_null() {
        return Err(fail(OtelStatus::InvalidArgument, field));
    }
    let bytes = if value.len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(value.ptr.cast::<u8>(), value.len) }
    };
    std::str::from_utf8(bytes).map_err(|_| fail(OtelStatus::InvalidUtf8, field))
}

fn stored_bytes(entries: &[BaggageEntry]) -> usize {
    entries
        .iter()
        .map(|e| e.key.len() + e.value.len() + e.metadata.len())
        .sum()
}

fn view_shape_is_valid(value: OtelStringView) -> bool {
    value.len == 0 || !value.ptr.is_null()
}

fn validate_bounds(entries: &[BaggageEntry]) -> Result<(), OtelStatus> {
    if entries.len() > MAX_BAGGAGE_ITEMS {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "baggage exceeds 64 entries",
        ));
    }
    if stored_bytes(entries) > MAX_BAGGAGE_BYTES {
        return Err(fail(
            OtelStatus::InvalidConfig,
            "baggage stored data exceeds 8192 bytes",
        ));
    }
    Ok(())
}

fn new_baggage(data: Arc<BaggageData>) -> *mut OtelBaggage {
    into_raw(OtelBaggage {
        header: OtelHandleHeader::new(OtelBaggage::KIND),
        data,
    })
}

pub(crate) fn baggage_from_data(data: Arc<BaggageData>) -> *mut OtelBaggage {
    new_baggage(data)
}

#[no_mangle]
pub extern "C" fn otel_baggage_builder_create() -> *mut OtelBaggageBuilder {
    guard_ptr(|| {
        clear_last_error();
        into_raw(OtelBaggageBuilder {
            header: OtelHandleHeader::new(OtelBaggageBuilder::KIND),
            entries: Vec::new(),
        })
    })
}

#[no_mangle]
/// # Safety
/// Builder and string views must remain valid and uniquely accessible for this call.
pub unsafe extern "C" fn otel_baggage_builder_set(
    builder: *mut OtelBaggageBuilder,
    key: OtelStringView,
    value: OtelStringView,
    metadata: OtelStringView,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let Some(builder) = (unsafe { checked_mut::<OtelBaggageBuilder>(builder) }) else {
            return OtelStatus::InvalidArgument;
        };
        if !view_shape_is_valid(key)
            || !view_shape_is_valid(value)
            || !view_shape_is_valid(metadata)
        {
            return fail(
                OtelStatus::InvalidArgument,
                "baggage input has a NULL pointer with non-zero length",
            );
        }
        let input_bytes = key
            .len
            .saturating_add(value.len)
            .saturating_add(metadata.len);
        if input_bytes > MAX_BAGGAGE_BYTES {
            return fail(
                OtelStatus::InvalidConfig,
                "baggage entry exceeds the 8192-byte storage limit",
            );
        }
        let key = match unsafe { string_from_view(key, "baggage key is invalid UTF-8") } {
            Ok(v) => v,
            Err(s) => return s,
        };
        if key.is_empty() {
            return fail(OtelStatus::InvalidArgument, "baggage key is empty");
        }
        let value = match unsafe { string_from_view(value, "baggage value is invalid UTF-8") } {
            Ok(v) => v,
            Err(s) => return s,
        };
        let metadata =
            match unsafe { string_from_view(metadata, "baggage metadata is invalid UTF-8") } {
                Ok(v) => v,
                Err(s) => return s,
            };
        let replacement = BaggageEntry {
            key,
            value,
            metadata,
        };
        let old = builder
            .entries
            .iter()
            .position(|e| e.key == replacement.key);
        let projected = stored_bytes(&builder.entries)
            - old.map_or(0, |i| {
                let e = &builder.entries[i];
                e.key.len() + e.value.len() + e.metadata.len()
            })
            + replacement.key.len()
            + replacement.value.len()
            + replacement.metadata.len();
        if projected > MAX_BAGGAGE_BYTES {
            return fail(
                OtelStatus::InvalidConfig,
                "baggage stored data exceeds 8192 bytes",
            );
        }
        if old.is_none() && builder.entries.len() == MAX_BAGGAGE_ITEMS {
            return fail(OtelStatus::InvalidConfig, "baggage exceeds 64 entries");
        }
        if let Some(i) = old {
            builder.entries[i] = replacement;
        } else {
            if builder.entries.try_reserve(1).is_err() {
                return fail(OtelStatus::InternalError, "baggage allocation failed");
            }
            builder.entries.push(replacement);
        }
        OtelStatus::Ok
    })
}

#[no_mangle]
/// # Safety
/// Builder and key storage must remain valid and uniquely accessible for this call.
pub unsafe extern "C" fn otel_baggage_builder_remove(
    builder: *mut OtelBaggageBuilder,
    key: OtelStringView,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let Some(builder) = (unsafe { checked_mut::<OtelBaggageBuilder>(builder) }) else {
            return OtelStatus::InvalidArgument;
        };
        let key = match unsafe { string_from_view(key, "baggage key is invalid UTF-8") } {
            Ok(v) => v,
            Err(s) => return s,
        };
        if let Some(i) = builder.entries.iter().position(|e| e.key == key) {
            builder.entries.remove(i);
        }
        OtelStatus::Ok
    })
}

#[no_mangle]
/// # Safety
/// Builder must be live and `out` must be writable.
pub unsafe extern "C" fn otel_baggage_builder_build(
    builder: *const OtelBaggageBuilder,
    out: *mut *mut OtelBaggage,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "baggage output is NULL");
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        let Some(builder) = (unsafe { checked_ref::<OtelBaggageBuilder>(builder) }) else {
            return OtelStatus::InvalidArgument;
        };
        if let Err(s) = validate_bounds(&builder.entries) {
            return s;
        }
        let mut entries = Vec::new();
        if entries.try_reserve_exact(builder.entries.len()).is_err() {
            return fail(OtelStatus::InternalError, "baggage allocation failed");
        }
        entries.extend(builder.entries.iter().cloned());
        unsafe {
            *out = new_baggage(Arc::new(BaggageData { entries }));
        }
        OtelStatus::Ok
    })
}

#[no_mangle]
/// # Safety
/// Builder must be NULL or a live owned handle not destroyed concurrently.
pub unsafe extern "C" fn otel_baggage_builder_destroy(builder: *mut OtelBaggageBuilder) {
    guard_unit(|| unsafe { destroy(builder) });
}

#[no_mangle]
/// # Safety
/// Baggage must be a live handle not destroyed concurrently.
pub unsafe extern "C" fn otel_baggage_clone(baggage: *const OtelBaggage) -> *mut OtelBaggage {
    guard_ptr(|| {
        clear_last_error();
        let Some(baggage) = (unsafe { checked_ref::<OtelBaggage>(baggage) }) else {
            return std::ptr::null_mut();
        };
        new_baggage(Arc::clone(&baggage.data))
    })
}

#[no_mangle]
/// # Safety
/// Baggage must be NULL or a live owned handle not destroyed concurrently.
pub unsafe extern "C" fn otel_baggage_destroy(baggage: *mut OtelBaggage) {
    guard_unit(|| unsafe { destroy(baggage) });
}

#[no_mangle]
/// # Safety
/// Baggage must be a live handle not destroyed concurrently.
pub unsafe extern "C" fn otel_baggage_count(baggage: *const OtelBaggage) -> usize {
    guard_value(0, || {
        clear_last_error();
        unsafe { checked_ref::<OtelBaggage>(baggage) }.map_or(0, |b| b.data.entries.len())
    })
}

#[no_mangle]
/// # Safety
/// Baggage/key must remain readable and `out` must point to a compatible writable view.
pub unsafe extern "C" fn otel_baggage_get(
    baggage: *const OtelBaggage,
    key: OtelStringView,
    out: *mut OtelBaggageEntryView,
) -> u32 {
    guard_value(0, || {
        clear_last_error();
        if out.is_null() {
            fail(OtelStatus::InvalidArgument, "baggage entry output is NULL");
            return 0;
        }
        let out_size = unsafe { out.cast::<usize>().read() };
        if out_size < BAGGAGE_ENTRY_VIEW_V1_SIZE {
            fail(
                OtelStatus::InvalidConfig,
                "baggage entry output struct_size is too small",
            );
            return 0;
        }
        let Some(baggage) = (unsafe { checked_ref::<OtelBaggage>(baggage) }) else {
            return 0;
        };
        let key = match unsafe { string_from_view(key, "baggage key is invalid UTF-8") } {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let Some(entry) = baggage.data.entries.iter().find(|e| e.key == key) else {
            return 0;
        };
        unsafe {
            *out = view(entry);
        }
        1
    })
}

#[no_mangle]
/// # Safety
/// Baggage must remain live and the callback/user data must be valid for synchronous use.
pub unsafe extern "C" fn otel_baggage_visit(
    baggage: *const OtelBaggage,
    visitor: OtelBaggageVisitor,
    user_data: *mut c_void,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        let Some(baggage) = (unsafe { checked_ref::<OtelBaggage>(baggage) }) else {
            return OtelStatus::InvalidArgument;
        };
        let Some(visitor) = visitor else {
            return fail(OtelStatus::InvalidArgument, "baggage visitor is NULL");
        };
        for entry in &baggage.data.entries {
            let entry = view(entry);
            let status = visitor(user_data, &entry);
            if status != OtelStatus::Ok {
                return status;
            }
        }
        OtelStatus::Ok
    })
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(bytes.len()).ok()?;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let h = (bytes[i + 1] as char).to_digit(16)?;
            let l = (bytes[i + 2] as char).to_digit(16)?;
            decoded.push(((h << 4) | l) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

fn has_valid_percent_encoding(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !(bytes[i + 1] as char).is_ascii_hexdigit()
                || !(bytes[i + 2] as char).is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}

fn parse_member(member: &str) -> Option<BaggageEntry> {
    let mut parts = member.split(';');
    let pair = parts.next()?.trim();
    let eq = pair.find('=')?;
    let key = pair[..eq].trim();
    let raw_value = pair[eq + 1..].trim();
    if key.is_empty() || !key.bytes().all(is_token_byte) {
        return None;
    }
    if !raw_value.bytes().all(|b| {
        b == b'%' || matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
    }) {
        return None;
    }
    let value = percent_decode(raw_value)?;
    let mut metadata = String::new();
    for property in parts {
        let property = property.trim();
        if property.is_empty() {
            return None;
        }
        let mut prop = property.splitn(2, '=');
        let k = prop.next()?;
        if k.is_empty() || !k.bytes().all(is_token_byte) {
            return None;
        }
        if let Some(v) = prop.next() {
            if !v.bytes().all(|b| {
                b == b'%'
                    || matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
            }) || !has_valid_percent_encoding(v)
            {
                return None;
            }
        }
        if !metadata.is_empty() {
            metadata.push(';');
        }
        metadata.push_str(property);
    }
    Some(BaggageEntry {
        key: key.to_owned(),
        value,
        metadata,
    })
}

fn encode_value(value: &str, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in value.as_bytes() {
        if matches!(b, 0x21 | 0x23..=0x24 | 0x26..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e) {
            output.push(b as char);
        } else {
            output.push('%');
            output.push(HEX[(b >> 4) as usize] as char);
            output.push(HEX[(b & 15) as usize] as char);
        }
    }
}

fn encoded_member(entry: &BaggageEntry) -> Option<String> {
    if entry.key.is_empty() || !entry.key.bytes().all(is_token_byte) {
        return None;
    }
    let mut out = String::new();
    out.try_reserve(entry.key.len() + entry.value.len() * 3 + entry.metadata.len() + 1)
        .ok()?;
    out.push_str(&entry.key);
    out.push('=');
    encode_value(&entry.value, &mut out);
    if !entry.metadata.is_empty() {
        // Metadata from extraction is wire-valid. Builder metadata is emitted only if it is a
        // syntactically valid W3C property list.
        let probe = format!("k=v;{}", entry.metadata);
        parse_member(&probe)?;
        out.push(';');
        out.push_str(&entry.metadata);
    }
    Some(out)
}

#[no_mangle]
/// # Safety
/// Header must be readable and `out` must be writable.
pub unsafe extern "C" fn otel_baggage_propagation_extract(
    header: OtelStringView,
    out: *mut *mut OtelBaggage,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out.is_null() {
            return fail(OtelStatus::InvalidArgument, "baggage output is NULL");
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if !view_shape_is_valid(header) {
            return fail(
                OtelStatus::InvalidArgument,
                "baggage header has a NULL pointer with non-zero length",
            );
        }
        let mut entries = Vec::new();
        if header.len <= MAX_BAGGAGE_BYTES {
            let header = match unsafe { str_from_view(header, "baggage header is invalid UTF-8") } {
                Ok(v) => v,
                Err(s) => return s,
            };
            if entries.try_reserve(MAX_BAGGAGE_ITEMS).is_err() {
                return fail(OtelStatus::InternalError, "baggage allocation failed");
            }
            for member in header.split(',') {
                let Some(entry) = parse_member(member) else {
                    continue;
                };
                if let Some(i) = entries
                    .iter()
                    .position(|e: &BaggageEntry| e.key == entry.key)
                {
                    entries[i] = entry;
                } else if entries.len() < MAX_BAGGAGE_ITEMS {
                    entries.push(entry);
                }
            }
        }
        unsafe {
            *out = new_baggage(Arc::new(BaggageData { entries }));
        }
        OtelStatus::Ok
    })
}

#[no_mangle]
/// # Safety
/// Baggage must be live, `out_len` writable, and a non-NULL buffer writable for capacity bytes.
pub unsafe extern "C" fn otel_baggage_propagation_inject(
    baggage: *const OtelBaggage,
    buffer: *mut std::ffi::c_char,
    capacity: usize,
    out_len: *mut usize,
) -> OtelStatus {
    guard_status(|| {
        clear_last_error();
        if out_len.is_null() {
            return fail(OtelStatus::InvalidArgument, "baggage output length is NULL");
        }
        let Some(baggage) = (unsafe { checked_ref::<OtelBaggage>(baggage) }) else {
            return OtelStatus::InvalidArgument;
        };
        let mut encoded = String::new();
        if encoded.try_reserve(MAX_BAGGAGE_BYTES).is_err() {
            return fail(OtelStatus::InternalError, "baggage allocation failed");
        }
        for entry in &baggage.data.entries {
            let Some(member) = encoded_member(entry) else {
                continue;
            };
            let separator = usize::from(!encoded.is_empty());
            if encoded.len() + separator + member.len() > MAX_BAGGAGE_BYTES {
                continue;
            }
            if separator != 0 {
                encoded.push(',');
            }
            encoded.push_str(&member);
        }
        unsafe {
            *out_len = encoded.len();
        }
        if buffer.is_null() {
            return if capacity == 0 {
                OtelStatus::Ok
            } else {
                fail(OtelStatus::InvalidArgument, "baggage output buffer is NULL")
            };
        }
        if capacity < encoded.len() {
            return fail(
                OtelStatus::InvalidArgument,
                "baggage output buffer is too small",
            );
        }
        unsafe {
            std::ptr::copy_nonoverlapping(encoded.as_ptr(), buffer.cast::<u8>(), encoded.len());
        }
        OtelStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> OtelStringView {
        sv(v)
    }

    #[test]
    fn build_lookup_clone_and_roundtrip_unicode_nul() {
        unsafe {
            let b = otel_baggage_builder_create();
            assert_eq!(
                otel_baggage_builder_set(b, s("tenant.id"), s("acme\0💼"), s("region=west")),
                OtelStatus::Ok
            );
            let mut bag = std::ptr::null_mut();
            assert_eq!(otel_baggage_builder_build(b, &mut bag), OtelStatus::Ok);
            assert_eq!(otel_baggage_count(bag), 1);
            let mut entry = OtelBaggageEntryView {
                struct_size: BAGGAGE_ENTRY_VIEW_V1_SIZE,
                key: s(""),
                value: s(""),
                metadata: s(""),
            };
            assert_eq!(otel_baggage_get(bag, s("tenant.id"), &mut entry), 1);
            let value = std::slice::from_raw_parts(entry.value.ptr.cast::<u8>(), entry.value.len);
            assert_eq!(value, "acme\0💼".as_bytes());
            let mut len = 0;
            assert_eq!(
                otel_baggage_propagation_inject(bag, std::ptr::null_mut(), 0, &mut len),
                OtelStatus::Ok
            );
            let mut wire = vec![0u8; len];
            assert_eq!(
                otel_baggage_propagation_inject(
                    bag,
                    wire.as_mut_ptr().cast(),
                    wire.len(),
                    &mut len
                ),
                OtelStatus::Ok
            );
            assert!(std::str::from_utf8(&wire).unwrap().contains("%00"));
            let mut decoded = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_propagation_extract(
                    OtelStringView {
                        ptr: wire.as_ptr().cast(),
                        len: wire.len()
                    },
                    &mut decoded
                ),
                OtelStatus::Ok
            );
            assert_eq!(otel_baggage_count(decoded), 1);
            otel_baggage_destroy(decoded);
            otel_baggage_destroy(bag);
            otel_baggage_builder_destroy(b);
        }
    }

    #[test]
    fn extraction_skips_bad_members_and_last_duplicate_wins() {
        unsafe {
            let mut bag = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_propagation_extract(
                    s("ok=one,bad member,ok=two,also=%GG,x=y"),
                    &mut bag
                ),
                OtelStatus::Ok
            );
            assert_eq!(otel_baggage_count(bag), 2);
            let mut entry: OtelBaggageEntryView = std::mem::zeroed();
            entry.struct_size = BAGGAGE_ENTRY_VIEW_V1_SIZE;
            assert_eq!(otel_baggage_get(bag, s("ok"), &mut entry), 1);
            assert_eq!(
                std::slice::from_raw_parts(entry.value.ptr.cast::<u8>(), entry.value.len),
                b"two"
            );
            otel_baggage_destroy(bag);
        }
    }

    #[test]
    fn extraction_replaces_non_utf8_percent_sequences_as_w3c_requires() {
        unsafe {
            let mut baggage = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_propagation_extract(s("key=%FF"), &mut baggage),
                OtelStatus::Ok
            );
            let mut entry: OtelBaggageEntryView = std::mem::zeroed();
            entry.struct_size = BAGGAGE_ENTRY_VIEW_V1_SIZE;
            assert_eq!(otel_baggage_get(baggage, s("key"), &mut entry), 1);
            assert_eq!(
                std::slice::from_raw_parts(entry.value.ptr.cast::<u8>(), entry.value.len),
                "�".as_bytes()
            );
            otel_baggage_destroy(baggage);
        }
    }

    #[test]
    fn oversized_remote_header_becomes_empty_baggage() {
        unsafe {
            let header = "a".repeat(MAX_BAGGAGE_BYTES + 1);
            let mut bag = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_propagation_extract(s(&header), &mut bag),
                OtelStatus::Ok
            );
            assert_eq!(otel_baggage_count(bag), 0);
            otel_baggage_destroy(bag);
        }
    }

    #[test]
    fn oversized_inputs_are_bounded_before_reading_caller_memory() {
        unsafe {
            let oversized = OtelStringView {
                ptr: std::ptr::NonNull::<std::ffi::c_char>::dangling().as_ptr(),
                len: MAX_BAGGAGE_BYTES + 1,
            };
            let builder = otel_baggage_builder_create();
            assert_eq!(
                otel_baggage_builder_set(builder, oversized, s("v"), s("")),
                OtelStatus::InvalidConfig
            );
            otel_baggage_builder_destroy(builder);

            let malformed = OtelStringView {
                ptr: std::ptr::null(),
                len: MAX_BAGGAGE_BYTES + 1,
            };
            let mut baggage = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_propagation_extract(malformed, &mut baggage),
                OtelStatus::InvalidArgument
            );
            assert!(baggage.is_null());
        }
    }

    #[test]
    fn logical_baggage_accepts_utf8_keys_but_inject_omits_unrepresentable_members() {
        unsafe {
            let builder = otel_baggage_builder_create();
            assert_eq!(
                otel_baggage_builder_set(builder, s("tenant id"), s("acme"), s("")),
                OtelStatus::Ok
            );
            assert_eq!(
                otel_baggage_builder_set(builder, s("valid"), s("kept"), s("prop=%GG")),
                OtelStatus::Ok
            );
            let mut baggage = std::ptr::null_mut();
            assert_eq!(
                otel_baggage_builder_build(builder, &mut baggage),
                OtelStatus::Ok
            );
            assert_eq!(otel_baggage_count(baggage), 2);

            let mut len = usize::MAX;
            assert_eq!(
                otel_baggage_propagation_inject(baggage, std::ptr::null_mut(), 0, &mut len),
                OtelStatus::Ok
            );
            assert_eq!(len, 0);

            otel_baggage_destroy(baggage);
            otel_baggage_builder_destroy(builder);
        }
    }
}
