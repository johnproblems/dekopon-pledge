//! Capability failures: stable codes, static messages, no input echo.

use std::fmt;

use dekopon_provider_sdk::provider::{Code, Failure, jsonl::StorageError};

/// Every way a pledge capability can fail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PledgeError {
    /// Settling an unknown or already-settled id.
    NotOpen,
    /// A `--due` value that is neither RFC 3339 nor a relative duration.
    BadDue,
    /// A `--within` value that is not a relative duration.
    BadWindow,
    /// A keep/drop id outside the minted `p-` plus six hex shape.
    BadId,
    /// Missing promise text.
    EmptyText,
    /// Missing or blank recipient.
    EmptyRecipient,
    /// Missing or blank drop reason.
    EmptyReason,
    /// A non-positive `--limit`.
    BadLimit,
    /// Piped promise text over 280 UTF-8 bytes.
    TextTooLong,
    /// Piped input that could not be read as text.
    StdinFailed,
    /// `due --fail-if-none` found nothing due: the watch-probe idle signal.
    NoneDue,
    /// `list --fail-if-none` found nothing matching.
    NoneFound,
    /// The ledger exceeds the 16 MiB in-memory fold bound.
    LedgerTooLarge,
    /// The broker clock reads past year 9999.
    ClockOutOfRange,
    /// A ledger line that does not parse or an inconsistent fold.
    LedgerCorrupt,
    /// A busy storage retry that lost twice, or a storage timeout: retry.
    StorageBusy,
    /// A denied, quota, or IO storage failure.
    StorageFailed,
    /// Output closed mid-write.
    OutputClosed,
}

impl fmt::Display for PledgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotOpen => "no open promise with that id",
            Self::BadDue => {
                "the due time is not RFC 3339 or a duration like 30s, 30m, 2h, 3d or 2w"
            }
            Self::BadWindow => "the window is not a duration like 30s, 30m, 2h, 3d or 2w",
            Self::BadId => "the promise id is not p- followed by six hex characters",
            Self::EmptyText => "promise text is required",
            Self::EmptyRecipient => "the recipient is missing",
            Self::EmptyReason => "a reason is required",
            Self::BadLimit => "the limit must be positive",
            Self::TextTooLong => "promise text exceeds 280 UTF-8 bytes",
            Self::StdinFailed => "piped input could not be read as text",
            Self::NoneDue => "no promise is due",
            Self::NoneFound => "no promise matches",
            Self::LedgerTooLarge => "the promise ledger exceeds the 16 MiB read bound",
            Self::ClockOutOfRange => "the broker clock is past year 9999",
            Self::LedgerCorrupt => "the promise ledger is corrupt",
            Self::StorageBusy => "promise storage is busy, retry the command",
            Self::StorageFailed => "promise storage operation failed",
            Self::OutputClosed => "stdout closed",
        })
    }
}

impl Failure for PledgeError {
    fn code(&self) -> Code {
        match self {
            Self::NotOpen => Code::new("not-open"),
            Self::BadDue => Code::new("bad-due").exiting(2),
            Self::BadWindow => Code::new("bad-window").exiting(2),
            Self::EmptyText
            | Self::EmptyRecipient
            | Self::EmptyReason
            | Self::BadLimit
            | Self::BadId => Code::USAGE,
            Self::TextTooLong => Code::new("text-too-long").exiting(2),
            Self::StdinFailed => Code::new("stdin-failed"),
            Self::NoneDue => Code::new("none-due"),
            Self::NoneFound => Code::new("none-found"),
            Self::LedgerTooLarge => Code::new("ledger-too-large"),
            Self::ClockOutOfRange => Code::new("clock-out-of-range"),
            Self::LedgerCorrupt => Code::new("ledger-corrupt"),
            Self::StorageBusy => Code::new("storage-busy"),
            Self::StorageFailed => Code::new("storage-failed"),
            Self::OutputClosed => Code::new("output-closed"),
        }
    }
}

/// Maps a host storage failure to a pledge failure.
#[must_use]
pub fn storage(error: StorageError) -> PledgeError {
    match error {
        StorageError::Corrupt => PledgeError::LedgerCorrupt,
        StorageError::Busy | StorageError::Timeout => PledgeError::StorageBusy,
        _ => PledgeError::StorageFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_with_usage_class_exits() {
        for (error, code, status) in [
            (PledgeError::NotOpen, "not-open", 1),
            (PledgeError::BadDue, "bad-due", 2),
            (PledgeError::BadWindow, "bad-window", 2),
            (PledgeError::BadId, "usage", 2),
            (PledgeError::EmptyText, "usage", 2),
            (PledgeError::EmptyRecipient, "usage", 2),
            (PledgeError::EmptyReason, "usage", 2),
            (PledgeError::BadLimit, "usage", 2),
            (PledgeError::TextTooLong, "text-too-long", 2),
            (PledgeError::StdinFailed, "stdin-failed", 1),
            (PledgeError::NoneDue, "none-due", 1),
            (PledgeError::NoneFound, "none-found", 1),
            (PledgeError::LedgerTooLarge, "ledger-too-large", 1),
            (PledgeError::ClockOutOfRange, "clock-out-of-range", 1),
            (PledgeError::LedgerCorrupt, "ledger-corrupt", 1),
            (PledgeError::StorageBusy, "storage-busy", 1),
            (PledgeError::StorageFailed, "storage-failed", 1),
            (PledgeError::OutputClosed, "output-closed", 1),
        ] {
            assert_eq!(error.code().as_str(), code);
            assert_eq!(error.code().status().get(), status);
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn storage_maps_busy_and_timeout_to_storage_busy() {
        assert_eq!(storage(StorageError::Corrupt), PledgeError::LedgerCorrupt);
        for error in [StorageError::Busy, StorageError::Timeout] {
            assert_eq!(storage(error), PledgeError::StorageBusy);
        }
        for error in [
            StorageError::NotFound,
            StorageError::AlreadyExists,
            StorageError::InvalidName,
            StorageError::InvalidArgument,
            StorageError::PermissionDenied,
            StorageError::QuotaExceeded,
            StorageError::Unsupported,
            StorageError::Io,
        ] {
            assert_eq!(storage(error), PledgeError::StorageFailed);
        }
    }
}
