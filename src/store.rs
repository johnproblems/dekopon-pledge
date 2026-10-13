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
/// not advance exactly to the observed size are ledger corruption, unless a
/// re-stat shows the stream grew mid-read — a concurrent append — in which
/// case the read restarts from offset 0 (at most twice).
pub fn read_ledger(store: &impl PledgeStore) -> Result<(u64, Vec<u8>), PledgeError> {
    let mut size = match store.size(LEDGER) {
        Ok(size) => size,
        Err(StorageError::NotFound) => return Ok((0, Vec::new())),
        Err(error) => return Err(storage(error)),
    };
    if size > MAX_LEDGER_BYTES {
        return Err(PledgeError::LedgerTooLarge);
    }
    let mut restarts = 0_u32;
    loop {
        let mut bytes = Vec::with_capacity(size.min(64 * 1024) as usize);
        let mut offset = 0;
        let mut overrun = false;
        while offset < size {
            let chunk = store
                .read_chunk(LEDGER, offset, READ_CHUNK)
                .map_err(storage)?;
            let advanced = chunk.next_offset.checked_sub(offset);
            if advanced != Some(chunk.bytes.len() as u64) || chunk.next_offset == offset {
                return Err(PledgeError::LedgerCorrupt);
            }
            if chunk.next_offset > size {
                overrun = true;
                break;
            }
            bytes.extend_from_slice(&chunk.bytes);
            offset = chunk.next_offset;
            if chunk.eof {
                break;
            }
        }
        if !overrun && offset == size && bytes.len() as u64 == size {
            return Ok((size, bytes));
        }
        let fresh = match store.size(LEDGER) {
            Ok(fresh) => fresh,
            Err(StorageError::NotFound) => return Err(PledgeError::LedgerCorrupt),
            Err(error) => return Err(storage(error)),
        };
        if fresh <= size || restarts >= 2 {
            return Err(PledgeError::LedgerCorrupt);
        }
        if fresh > MAX_LEDGER_BYTES {
            return Err(PledgeError::LedgerTooLarge);
        }
        size = fresh;
        restarts += 1;
    }
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

    /// A store that appends one line on the first chunk read, racing the
    /// snapshotted size the way a concurrent writer would.
    struct GrowingStore<'a> {
        inner: &'a FakeStore,
        extra: Vec<u8>,
        grown: std::cell::Cell<bool>,
    }

    impl PledgeStore for GrowingStore<'_> {
        fn size(&self, name: &str) -> Result<u64, StorageError> {
            self.inner.size(name)
        }

        fn read_chunk(
            &self,
            name: &str,
            offset: u64,
            max_bytes: u32,
        ) -> Result<Chunk, StorageError> {
            if !self.grown.get() {
                self.grown.set(true);
                self.inner.data.borrow_mut().extend_from_slice(&self.extra);
            }
            self.inner.read_chunk(name, offset, max_bytes)
        }

        fn append(&self, name: &str, expected: u64, record: &[u8]) -> Result<u64, StorageError> {
            self.inner.append(name, expected, record)
        }
    }

    /// A store whose first size report is inflated, then shrinks to the real
    /// length on re-stat.
    struct ShrinkingStore<'a> {
        inner: &'a FakeStore,
        lied: std::cell::Cell<bool>,
    }

    impl PledgeStore for ShrinkingStore<'_> {
        fn size(&self, _: &str) -> Result<u64, StorageError> {
            let real = self.inner.data.borrow().len() as u64;
            if self.lied.get() {
                Ok(real)
            } else {
                self.lied.set(true);
                Ok(real + 100)
            }
        }

        fn read_chunk(
            &self,
            name: &str,
            offset: u64,
            max_bytes: u32,
        ) -> Result<Chunk, StorageError> {
            self.inner.read_chunk(name, offset, max_bytes)
        }

        fn append(&self, name: &str, expected: u64, record: &[u8]) -> Result<u64, StorageError> {
            self.inner.append(name, expected, record)
        }
    }

    /// A store that ends the stream early with an internally consistent but
    /// short final chunk.
    struct TruncatingStore<'a> {
        inner: &'a FakeStore,
    }

    impl PledgeStore for TruncatingStore<'_> {
        fn size(&self, name: &str) -> Result<u64, StorageError> {
            self.inner.size(name)
        }

        fn read_chunk(&self, _: &str, offset: u64, _: u32) -> Result<Chunk, StorageError> {
            let data = self.inner.data.borrow();
            let start = offset as usize;
            let end = (start + data.len() / 2).min(data.len());
            Ok(Chunk {
                bytes: data[start..end].to_vec(),
                next_offset: end as u64,
                eof: true,
            })
        }

        fn append(&self, name: &str, expected: u64, record: &[u8]) -> Result<u64, StorageError> {
            self.inner.append(name, expected, record)
        }
    }

    #[test]
    fn read_ledger_restarts_when_the_stream_grows_mid_read() {
        let first = br#"{"v":1,"ev":"made","id":"p-000001","at":"2026-10-13T00:00:00.000Z","to":"a","due":"2026-10-14T00:00:00.000Z","text":"t"}"#.as_slice();
        let second = br#"{"v":1,"ev":"made","id":"p-000002","at":"2026-10-13T00:00:00.000Z","to":"b","due":"2026-10-14T00:00:00.000Z","text":"u"}"#.as_slice();
        let inner = FakeStore::new();
        inner.data.borrow_mut().extend_from_slice(first);
        inner.data.borrow_mut().push(b'\n');
        let mut extra = Vec::from(second);
        extra.push(b'\n');
        let store = GrowingStore {
            inner: &inner,
            extra,
            grown: std::cell::Cell::new(false),
        };
        let (size, bytes) = read_ledger(&store).unwrap();
        let mut expected = Vec::from(first);
        expected.push(b'\n');
        expected.extend_from_slice(second);
        expected.push(b'\n');
        assert_eq!(size as usize, expected.len());
        assert_eq!(bytes, expected);
        assert_eq!(crate::model::fold(&bytes).unwrap().len(), 2);
    }

    #[test]
    fn read_ledger_still_cries_corrupt_when_the_size_does_not_grow() {
        let line = br#"{"v":1,"ev":"made","id":"p-000001","at":"2026-10-13T00:00:00.000Z","to":"a","due":"2026-10-14T00:00:00.000Z","text":"t"}"#.as_slice();
        let inner = FakeStore::new();
        inner.data.borrow_mut().extend_from_slice(line);
        inner.data.borrow_mut().push(b'\n');
        let shrinking = ShrinkingStore {
            inner: &inner,
            lied: std::cell::Cell::new(false),
        };
        assert_eq!(read_ledger(&shrinking), Err(PledgeError::LedgerCorrupt));
        let truncating = TruncatingStore { inner: &inner };
        assert_eq!(read_ledger(&truncating), Err(PledgeError::LedgerCorrupt));
    }

    #[test]
    fn read_ledger_reads_a_large_stream_across_chunks() {
        let store = FakeStore::new();
        *store.data.borrow_mut() = vec![b'x'; 300 * 1024];
        let (size, bytes) = read_ledger(&store).unwrap();
        assert_eq!(size, 300 * 1024);
        assert_eq!(bytes.len(), 300 * 1024);
    }
}
