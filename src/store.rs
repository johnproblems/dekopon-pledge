//! Ledger storage: one append-only JSONL stream behind a narrow trait.
//!
//! [`PledgeStore`] keeps the capability logic testable without a component
//! host: production code uses [`Storage<Jsonl>`], unit tests use an in-memory
//! fake. Every write is a single conditional append, the only atomic unit the
//! host guarantees.

use dekopon_provider_sdk::provider::{
    Jsonl, Storage,
    jsonl::{Chunk, StorageError},
};

use crate::error::{PledgeError, storage};

/// The broker-owned JSONL stream holding the event log.
pub const LEDGER: &str = "pledges.jsonl";

/// One host read; small enough for any quota, large enough for few round trips.
const READ_CHUNK: u32 = 256 * 1024;

/// Refuse to fold a ledger larger than this into memory.
const MAX_LEDGER_BYTES: u64 = 16 * 1024 * 1024;

/// The three JSONL operations pledge needs.
pub trait PledgeStore {
    /// Returns the stream size in bytes.
    fn size(&self, name: &str) -> Result<u64, StorageError>;
    /// Reads up to `max_bytes` from `offset`.
    fn read_chunk(&self, name: &str, offset: u64, max_bytes: u32) -> Result<Chunk, StorageError>;
    /// Appends one newline-free JSON record when the size still matches.
    fn append(&self, name: &str, expected_size: u64, record: &[u8]) -> Result<u64, StorageError>;
}

impl PledgeStore for Storage<Jsonl> {
    fn size(&self, name: &str) -> Result<u64, StorageError> {
        self.size(name)
    }

    fn read_chunk(&self, name: &str, offset: u64, max_bytes: u32) -> Result<Chunk, StorageError> {
        self.read_chunk(name, offset, max_bytes)
    }

    fn append(&self, name: &str, expected_size: u64, record: &[u8]) -> Result<u64, StorageError> {
        self.append(name, expected_size, record)
    }
}

/// Reads the whole ledger, returning its size and bytes.
///
/// A missing stream is an empty ledger, not an error. Chunk offsets that do
/// not advance exactly to the observed size are ledger corruption.
pub fn read_ledger(store: &impl PledgeStore) -> Result<(u64, Vec<u8>), PledgeError> {
    let size = match store.size(LEDGER) {
        Ok(size) => size,
        Err(StorageError::NotFound) => return Ok((0, Vec::new())),
        Err(error) => return Err(storage(error)),
    };
    if size > MAX_LEDGER_BYTES {
        return Err(PledgeError::LedgerTooLarge);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    let mut offset = 0;
    while offset < size {
        let chunk = store
            .read_chunk(LEDGER, offset, READ_CHUNK)
            .map_err(storage)?;
        let advanced = chunk.next_offset.checked_sub(offset);
        if advanced != Some(chunk.bytes.len() as u64)
            || chunk.next_offset == offset
            || chunk.next_offset > size
        {
            return Err(PledgeError::LedgerCorrupt);
        }
        bytes.extend_from_slice(&chunk.bytes);
        offset = chunk.next_offset;
        if chunk.eof {
            break;
        }
    }
    if offset != size || bytes.len() as u64 != size {
        return Err(PledgeError::LedgerCorrupt);
    }
    Ok((size, bytes))
}

/// Appends one record, retrying once on a lost size race.
///
/// The host reports a moved size as [`StorageError::Busy`]; the retry
/// re-reads the size, so callers that validated against ledger content must
/// re-validate too (see `transact` in the crate root).
pub fn append_with_retry(store: &impl PledgeStore, record: &[u8]) -> Result<u64, PledgeError> {
    for _ in 0..2 {
        let size = match store.size(LEDGER) {
            Ok(size) => size,
            Err(StorageError::NotFound) => 0,
            Err(error) => return Err(storage(error)),
        };
        match store.append(LEDGER, size, record) {
            Ok(next) => return Ok(next),
            Err(StorageError::Busy) => continue,
            Err(error) => return Err(storage(error)),
        }
    }
    Err(PledgeError::StorageFailed)
}

#[cfg(test)]
pub(crate) struct FakeStore {
    pub(crate) data: std::cell::RefCell<Vec<u8>>,
    pub(crate) busy_appends: std::cell::Cell<u32>,
    pub(crate) deny_append: bool,
}

#[cfg(test)]
impl FakeStore {
    pub(crate) fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(Vec::new()),
            busy_appends: std::cell::Cell::new(0),
            deny_append: false,
        }
    }
}

#[cfg(test)]
impl PledgeStore for FakeStore {
    fn size(&self, _: &str) -> Result<u64, StorageError> {
        Ok(self.data.borrow().len() as u64)
    }

    fn read_chunk(&self, _: &str, offset: u64, max_bytes: u32) -> Result<Chunk, StorageError> {
        let data = self.data.borrow();
        if offset > data.len() as u64 {
            return Err(StorageError::InvalidArgument);
        }
        let start = offset as usize;
        let end = (start + max_bytes as usize).min(data.len());
        Ok(Chunk {
            bytes: data[start..end].to_vec(),
            next_offset: end as u64,
            eof: end == data.len(),
        })
    }

    fn append(&self, _: &str, expected: u64, record: &[u8]) -> Result<u64, StorageError> {
        if self.deny_append {
            return Err(StorageError::PermissionDenied);
        }
        if self.busy_appends.get() > 0 {
            self.busy_appends.set(self.busy_appends.get() - 1);
            return Err(StorageError::Busy);
        }
        let mut data = self.data.borrow_mut();
        if data.len() as u64 != expected {
            return Err(StorageError::Busy);
        }
        if record.contains(&b'\n')
            || record.contains(&b'\r')
            || serde_json::from_slice::<serde_json::Value>(record).is_err()
        {
            return Err(StorageError::InvalidArgument);
        }
        data.extend_from_slice(record);
        data.push(b'\n');
        Ok(data.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ledger_treats_a_missing_stream_as_empty() {
        let store = FakeStore::new();
        assert_eq!(read_ledger(&store).unwrap(), (0, Vec::new()));
    }

    #[test]
    fn read_ledger_refuses_streams_past_the_fold_bound() {
        let store = FakeStore::new();
        *store.data.borrow_mut() = vec![b'x'; 16 * 1024 * 1024 + 1];
        assert_eq!(read_ledger(&store), Err(PledgeError::LedgerTooLarge));
    }

    #[test]
    fn append_retries_one_busy_then_reports_storage_failed() {
        let store = FakeStore::new();
        store.busy_appends.set(1);
        let record = br#"{"v":1,"ev":"made","id":"p-000001","at":"2026-10-13T00:00:00.000Z","to":"a","due":"2026-10-14T00:00:00.000Z","text":"t"}"#;
        let size = append_with_retry(&store, record).unwrap();
        assert_eq!(size as usize, record.len() + 1);

        store.busy_appends.set(2);
        assert_eq!(
            append_with_retry(&store, record),
            Err(PledgeError::StorageFailed)
        );
    }

    #[test]
    fn read_only_denial_maps_to_storage_failed_and_writes_nothing() {
        let mut store = FakeStore::new();
        store.deny_append = true;
        assert_eq!(
            append_with_retry(&store, br#"{"a":1}"#),
            Err(PledgeError::StorageFailed)
        );
        assert!(store.data.borrow().is_empty());
    }
}
