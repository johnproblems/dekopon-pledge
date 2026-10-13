# Changelog

## 0.2.0 — 2026-10-13

Capability and UX additions (no new capabilities):

- `s` and `w` duration units (`30s`, `2w`) for `--due` and `--within`.
- `pledge due --limit N` (default 100) and `pledge due --to <who>`.
- `pledge list --oldest-first` and `pledge list --fail-if-none`.
- `--note` and `--reason` accept multiple words without quotes.
- Past-due makes explicitly allowed, echoing `"overdue":true`.

Error taxonomy:

- New `none-found` (list), `ledger-too-large`, and `clock-out-of-range`
  codes; the read bound and clock faults no longer hide in `storage-failed`.

Verification:

- Stdin end-to-end (text, empty, overlong, non-UTF8, absent).
- `run-command` end-to-end through the real component.
- Due-edge, corrupt-ledger, and id-uniqueness end-to-end suites.
- Deterministic 10k-case time-parser fuzz loop; multibyte boundary tests.
- CI runs both daemons' `check` on the example deployment, plus
  `cargo-deny` and `cargo-machete`.

Ops and docs:

- `make install/uninstall PROVIDERS_DIR=...`; build writes a
  `pledge.wasm.sha256` sidecar.
- README semantics section, jq cookbook, and the exact `wake watch` payload.

## 0.1.0 — 2026-10-13

Initial release: the pledge commitments ledger provider.

- Six capabilities under provider id `pledge`: make, list, due, keep, drop,
  stats. Writes are `LocalWrite`, reads `ReadOnly`, all `Low` risk.
- Append-only JSONL event log in broker-owned storage, folded on read;
  `broken` derived, never stored; single conditional append per write with one
  race retry and full re-validation.
- Time from the broker wall clock (RFC 3339 or `Nm/Nh/Nd`, resolved in
  `run`), ids from broker entropy, closed input schemas, JSON-line output.
- `pledge due --fail-if-none`: exits 1 with `none-due` while nothing is due,
  so watch probes can tell "due" from "quiet". Stays `ReadOnly`.
- Storage end-to-end suite against the real component and real on-disk
  storage, including restart survival and read-only host rejection.
- Runnable broker + gateway example deployment over a `kind: local` socket.
