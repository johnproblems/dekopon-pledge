# Changelog

## 0.3.0 — 2026-10-13

Capability and UX additions (no new capabilities):

- `pledge list --kept` / `--dropped` filters; `--settled` still means
  either.
- `pledge list --overdue`, composable with `--to`, `--open`, `--limit`,
  and `--fail-if-none`.
- `pledge stats --by-recipient`: one sorted JSON line per recipient.
- `--version` on `pledge` and every subcommand, rendered purely in
  `propose`.
- Every promise in `list`/`keep` output carries `"late"` (kept strictly
  after due).

Error taxonomy:

- New `bad-window` (exit 2) for malformed `--within`; `bad-due`
  unchanged.
- New `storage-busy` (exit 1, retry hint) for exhausted Busy/Timeout;
  denials/quota/IO stay `storage-failed`.
- New `BadId` usage error (exit 2, static message) for malformed
  keep/drop ids, checked before any import read.

Fold and parsing hardening:

- Ledger reads retry across concurrent appends (max 2 restarts);
  allocation capped at 64 KiB before growing; 16 MiB gate unchanged.
- Fold rejects non-`p-`+6-hex ids, time-travel settlements, and
  blank-after-trim text as `ledger-corrupt`.
- `parse_duration` is char-boundary safe (no panic on multibyte
  suffixes).
- Absolute-stamp `OutOfRange` preserved through `resolve_due`;
  write-side timestamp failures map to `clock-out-of-range`.
- Removed `pub append_with_retry`; all writes go through re-validating
  `transact`.

Verification:

- Run-command e2e covers all six capabilities in-wasm; new drop
  round-trip, corrupt-write, and fault-code e2e suites.
- Grammar/manifest tests pin every `--help`, `--version`, and required
  schema field.
- Time-parser fuzz corpus adds range edges and multibyte soup with
  revert sensitivity.
- New secrets-guard test locks `.crates.env` out of git.

Ops and docs:

- Published crate file list constrained (`include`); PRD moved to
  `docs/PRD.md`.
- `make test` unsets the component var; `make clean`/`check` header
  fixed.
- CI consolidated on a single cached `make check`; actions pinned to
  SHAs with read-scoped token, concurrency cancel, 30 min timeout.
- Build script fails on wasm-tools mismatch (override with
  `ALLOW_WASM_TOOLS_MISMATCH=1`); portable sha256 probe.
- Example deployment synced with the grammar and 16 MiB bound,
  validated by both daemons' `check`.

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
