//! SDK-side error helpers. Diagnostics are recorded in the **API-owned** thread-local slot
//! (via the internal ABI in [`crate::api_ffi`]) so a subsequent `otel_last_error_message()`
//! returns them, exactly as in the single-crate build.

use opentelemetry_c_abi::{AbiError, OtelStatus};
use opentelemetry_sdk::error::OTelSdkError;
use std::cell::Cell;

use crate::api_ffi;

thread_local! {
    static LAST_STATUS: Cell<OtelStatus> = const { Cell::new(OtelStatus::Ok) };
}

/// Record `message` in the API error slot and return `status`.
pub(crate) fn fail(status: OtelStatus, message: &str) -> OtelStatus {
    LAST_STATUS.with(|slot| slot.set(status));
    api_ffi::set_last_error(message);
    status
}

/// Record an owned `message` and return `status`.
pub(crate) fn fail_owned(status: OtelStatus, message: String) -> OtelStatus {
    LAST_STATUS.with(|slot| slot.set(status));
    api_ffi::set_last_error(&message);
    status
}

/// Clear the API error slot (called at the start of fallible entry points).
pub(crate) fn clear_last_error() {
    reset_last_status();
    api_ffi::clear_last_error();
}

pub(crate) fn reset_last_status() {
    LAST_STATUS.with(|slot| slot.set(OtelStatus::Ok));
}

pub(crate) fn last_status_or(fallback: OtelStatus) -> OtelStatus {
    LAST_STATUS.with(|slot| {
        let status = slot.get();
        if status == OtelStatus::Ok {
            fallback
        } else {
            status
        }
    })
}

/// Map an [`AbiError`] onto a status, recording its message.
pub(crate) fn fail_abi(err: AbiError) -> OtelStatus {
    fail(err.status, err.message)
}

/// Map an SDK export-pipeline lifecycle failure onto a status and record its detail.
///
/// Upstream uses `InternalFailure` for exporter failures surfaced by force-flush/shutdown.
/// At this public boundary those are export-pipeline failures, not failures of the C wrapper
/// itself. Wrapper panics, allocation failures, and worker creation failures use
/// [`OtelStatus::InternalError`] at their source.
pub(crate) fn status_from_export_pipeline_error(err: &OTelSdkError) -> OtelStatus {
    match err {
        OTelSdkError::AlreadyShutdown => fail(
            OtelStatus::AlreadyShutdown,
            "operation failed: provider already shut down",
        ),
        OTelSdkError::Timeout(d) => fail_owned(
            OtelStatus::Timeout,
            format!("operation timed out after {d:?}"),
        ),
        OTelSdkError::InternalFailure(msg) => {
            fail_owned(OtelStatus::ExportFailed, format!("internal failure: {msg}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_pipeline_error_statuses_follow_public_policy() {
        assert_eq!(
            status_from_export_pipeline_error(&OTelSdkError::AlreadyShutdown),
            OtelStatus::AlreadyShutdown
        );
        assert_eq!(
            status_from_export_pipeline_error(&OTelSdkError::Timeout(
                std::time::Duration::from_millis(1)
            )),
            OtelStatus::Timeout
        );
        assert_eq!(
            status_from_export_pipeline_error(&OTelSdkError::InternalFailure(
                "exporter rejected batch".to_owned()
            )),
            OtelStatus::ExportFailed
        );
    }
}
