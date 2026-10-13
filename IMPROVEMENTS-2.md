# Pledge improvements (second batch)

Thirty scoped, verifiable improvements in five batches of six, ordered
foundational-first. No new capabilities, no new broker imports, no
scheduler, no compact/snooze. Each item ships with its test or check.

Conventions: SPEC is the concrete change; ACCEPTANCE is the check that
proves it; FILES names the files touched. "e2e" means the
`tests/ledger.rs` storage suite on `BrokerProviderRegistry::load_with_storage`
with `DEKOPON_PROVIDER_COMPONENT` set, per AGENTS.md.

## Batch A — Guest read-path correctness

### 1. Retry ledger reads across concurrent appends
SPEC: In `read_ledger`, when chunk offsets miss the snapshotted size
(`next_offset > size`, or final `offset != size`), re-stat the stream
before crying corrupt: if the size grew, restart the read from offset 0
(at most 2 restarts); report `LedgerCorrupt` only when the size is
unchanged or shrank, or bytes fail to parse.
ACCEPTANCE: A `FakeStore` that appends one valid line mid-read folds
successfully; a truncated stream and a shrunk size still yield
`ledger-corrupt`.
FILES: `src/store.rs`.

### 2. Cap the initial ledger read allocation
SPEC: Replace `Vec::with_capacity(size as usize)` with
`Vec::with_capacity(min(size, 64 * 1024))` and grow via `extend`; keep
the 16 MiB `MAX_LEDGER_BYTES` gate unchanged.
ACCEPTANCE: Full-ledger reads still pass; a large-but-legal stream reads
correctly (allocation shape verified by code inspection).
FILES: `src/store.rs`.

### 3. Enforce the strict promise-id shape in fold
SPEC: In `apply()` for `Made`, reject ids that are not exactly `p-`
plus six lowercase hex characters as `LedgerCorrupt`, instead of the
current nonempty-and-`<= MAX_TO_BYTES` bound.
ACCEPTANCE: Fold rejects `P-7F3A9C`, `p-xyz`, `p-7f3a9c00`, and overlong
ids; existing valid fixtures still fold.
FILES: `src/model.rs`.

### 4. Reject time-travel settlements in fold
SPEC: In `apply()` for `Kept`/`Dropped`, return `LedgerCorrupt` when the
settlement `at_ms` is strictly less than the promise's `made_at_ms`.
Equality stays legal (settle in the same millisecond as make).
ACCEPTANCE: Ledger `[made@T, kept@(T-1)]` yields `ledger-corrupt`;
`kept@T` still folds.
FILES: `src/model.rs`.

### 5. Reject blank-after-trim text fields in fold
SPEC: In `apply()`, reject `to`/`text`/`reason`/`note` values that are
empty after `trim` as `LedgerCorrupt` (trim check only; store the
original). This matches the `run`-side blank rejection no capability
could have written past.
ACCEPTANCE: A `made` line with `"to": "   "` or `"text": "\t"` yields
`ledger-corrupt`.
FILES: `src/model.rs`.

### 6. Fix the `parse_duration` panic on multibyte suffixes
SPEC: Split the unit suffix with a char-boundary-safe method (e.g. match
on `text.chars().last()` and slice by its `len_utf8`) instead of
`split_at(text.len().saturating_sub(1))`, which panics inside a
multibyte char on user-controlled argv.
ACCEPTANCE: `parse_duration("2é")`, `("é")`, and `("h\u{0301}")` all
return `TimeError::Invalid` without panicking.
FILES: `src/time.rs`.

## Batch B — Error taxonomy and API surface

### 7. Map write-side timestamp format failures to clock-out-of-range
SPEC: In `made_line`/`kept_line`/`dropped_line`, map `format_rfc3339`
failure (faulty caller/clock input, ledger untouched) to
`ClockOutOfRange` instead of `LedgerCorrupt`. Keep `promise_value`'s
mapping: data read back from the ledger is correctly corrupt.
ACCEPTANCE: `made_line(.., due_ms = MAX_MILLIS + 1, ..)` returns
`ClockOutOfRange`, not `LedgerCorrupt`.
FILES: `src/model.rs`.

### 8. Preserve OutOfRange for absolute timestamps in resolve_due
SPEC: In `resolve_due`, return `OutOfRange` immediately when
`parse_rfc3339` fails with `OutOfRange` (e.g. year 0001) instead of
falling through to duration parsing, which re-labels it `Invalid`.
Durations never contain `-`/colons/`T` separators, so no valid duration
is lost.
ACCEPTANCE: `resolve_due("0001-01-01T00:00:00Z", _)` is `OutOfRange`;
`"tomorrow"` is still `Invalid`; run-level `BadDue` mapping unchanged.
FILES: `src/time.rs`.

### 9. Give --within failures their own bad-window code
SPEC: Add `Code::new("bad-window").exiting(2)` for `BadWindow` (message
kept: "the window is not a duration like 30s, 30m, 2h, 3d or 2w"),
update the README failure table and the `codes_are_stable` test.
Merges the duplicate proposal that `BadDue`/`BadWindow` share a code.
ACCEPTANCE: Malformed `--within` yields code `bad-window`, exit 2;
malformed `--due` still yields `bad-due`.
FILES: `src/error.rs`, `README.md`.

### 10. Split transient storage-busy from permanent storage-failed
SPEC: Map exhausted `Busy` retries and `Timeout` to a new `storage-busy`
code (exit 1, "promise storage is busy, retry the command"); keep
denials/quota/IO as `storage-failed`. Update the README failure table
and the storage-mapping test.
ACCEPTANCE: A `FakeStore` with 2 forced `Busy` appends yields
`storage-busy`; `deny_append` still yields `storage-failed` and writes
nothing.
FILES: `src/error.rs`, `src/store.rs`, `src/lib.rs`, `README.md`.

### 11. Validate keep/drop id shape as usage, not not-open
SPEC: Add `PledgeError::BadId` with `Code::USAGE` (exit 2, static
message "the promise id is not p- followed by six hex characters", no
input echo), checked at the top of `Keep`/`Drop` `run` before any
storage read. Merges the duplicate proposal that keep/drop inputs
accept any id string. Complements item 3 (fold-side) with a run-side
usage error.
ACCEPTANCE: `keep`/`drop` with `"xyz"`/`"p-ZZZZZZ"`/`""` exit 2 and
perform zero storage reads/writes; unknown-but-well-formed ids still
yield `not-open`.
FILES: `src/error.rs`, `src/lib.rs`, `src/cli.rs`, `tests/ledger.rs`.

### 12. Remove the content-unsafe pub append_with_retry
SPEC: Delete `append_with_retry`, or make it `#[cfg(test)]`/private, or
change its signature to take a re-validation closure over the re-read
ledger. Production `transact` already re-implements the loop with full
re-validation; the `pub` helper appends without re-folding.
ACCEPTANCE: No `pub` write helper appends without re-fold; existing
`transact` tests pass unchanged.
FILES: `src/store.rs`, `src/lib.rs`.

## Batch C — Capability UX (no new capabilities)

### 13. Split list --settled into --kept and --dropped
SPEC: Add `StateFilter::Kept`/`Dropped` (`"kept"`/`"dropped"` in the
closed `ListInput` schema) with `--kept`/`--dropped` flags conflicting
with `--open`/`--settled`; filter on the `Settled` variant in `List`
`run`. `--settled` keeps meaning kept-or-dropped.
ACCEPTANCE: `--kept` hides dropped promises and vice versa; `--settled`
output unchanged; manifest test pins the new enum values.
FILES: `src/input.rs`, `src/cli.rs`, `src/lib.rs`, `tests/cli.rs`.

### 14. Add list --overdue
SPEC: Add an `overdue_only` bool to `ListInput` (default false) with a
`--overdue` flag; filter with the existing `Promise::is_overdue(now_ms)`
in `List` `run`. Composable with `--to`, `--open`, `--limit`, and
`--fail-if-none`.
ACCEPTANCE: `--overdue` lists exactly the open past-due promises;
combines with `--to`; empty match with `--fail-if-none` yields
`none-found`.
FILES: `src/input.rs`, `src/cli.rs`, `src/lib.rs`, `tests/cli.rs`.

### 15. Add stats --by-recipient
SPEC: Add a `by_recipient` bool to `StatsInput` with a `--by-recipient`
flag; emit one compact JSON object per recipient line
(`{"to": ..., "open": ..., "kept": ..., "dropped": ..., "broken": ...,
"kept_late": ..., "on_time_rate": ...}`), sorted by recipient, replacing
the `jq group_by` cookbook workaround. Default aggregate output
unchanged.
ACCEPTANCE: Two-recipient ledger yields two sorted lines with correct
per-recipient counts; `--to` still narrows to the aggregate object.
FILES: `src/input.rs`, `src/cli.rs`, `src/lib.rs`, `README.md`,
`tests/ledger.rs`.

### 16. Add --version to the pledge grammar
SPEC: Set the clap `version` to the crate version on `Args` so
`pledge --version` and `pledge <subcommand> --version` render it purely
in `propose` with no import reads.
ACCEPTANCE: `--version` prints the crate version and exits 0 in native
and run-command e2e; no storage reads/writes occur.
FILES: `src/cli.rs`, `tests/cli.rs`, `tests/ledger.rs`.

### 17. Surface kept-late on each kept promise
SPEC: In `promise_value`, always emit `"late"`: true when the promise
was kept strictly after its due time (`was_kept_late`, today only
visible in the aggregate `kept_late` count), false otherwise. Document
the field in the README semantics section.
ACCEPTANCE: A promise kept after its due time renders `"late": true` in
`list`/`keep` output; open and on-time-kept promises render false.
FILES: `src/model.rs`, `README.md`, `tests/ledger.rs`.

### 18. Sync the example deployment with the 0.2.0 grammar and bounds
SPEC: In `examples/broker/dekopon.yaml`, extend the standing-orders time
syntax to `30s` and `2w` and mention the new flags (`--to`, `--limit`,
`--oldest-first`, `--fail-if-none`); in `broker.yaml`, add a comment
coupling `maxFileBytes`/`maxReadBytesPerInvocation` (16 MiB) to the
guest `MAX_LEDGER_BYTES` fold bound. Validate with both daemons'
`check` subcommands from `examples/broker`.
ACCEPTANCE: `dekopon-brokerd check broker.yaml &&
dekopon-gatewayd check gatewayd.yaml` pass; grep shows no stale unit
list; the coupling comment names both sides.
FILES: `examples/broker/dekopon.yaml`, `examples/broker/broker.yaml`.

## Batch D — Verification depth

### 19. Extend run-command e2e to keep/drop/list/stats
SPEC: Extend `run_command_through_component_matches_native_proposals`
with propose-in-wasm cases for `keep`, `drop` (incl. multi-word reason),
`list` (incl. `--kept`/`--overdue` once items 13-14 land), and `stats`,
plus a malformed-id refusal case once item 11 lands.
ACCEPTANCE: All six capabilities propose through the real component;
refusals match native `propose` codes.
FILES: `tests/ledger.rs`.

### 20. Add a drop round-trip e2e
SPEC: Mirror `make_list_keep_stats_round_trip` with a
make-list-drop-stats round trip: two makes, drop one with a reason,
assert `list` shows `dropped` with the reason, `stats` counts it, and
the survivor stays open.
ACCEPTANCE: New test passes against the real component and real storage;
settling twice still fails `not-open` without writing.
FILES: `tests/ledger.rs`.

### 21. Fail writes loudly on a corrupt ledger e2e
SPEC: Extend the corrupt-ledger e2e (which today asserts
`list`/`due`/`stats` reads only) with `make`/`keep`/`drop` attempts
against the planted-garbage ledger: each exits 1 with
`the promise ledger is corrupt`, emits no stdout, and appends nothing
(byte-identical ledger file afterwards).
ACCEPTANCE: All three writes fail corrupt and the ledger file is
untouched; reads still fail as before.
FILES: `tests/ledger.rs`.

### 22. Cover the fault codes in-wasm e2e
SPEC: Add in-wasm e2e for the two fault codes pinned only by unit tests
today: drive a clock past year 9999 via the harness clock helper and
expect `clock-out-of-range` from a write; plant a `> 16 MiB` ledger file
via the `find_ledger_file` pattern and expect `ledger-too-large` from a
read.
ACCEPTANCE: Both codes observed through the real component with the
stable messages; normal operations beside them still succeed.
FILES: `tests/ledger.rs`.

### 23. Harden the grammar and manifest tests
SPEC: In `tests/cli.rs`, pin every subcommand's `--help` rendering
(status 0, names the subcommand), the new `--version` output, and the
required-field shape of all six input schemas (today only `make`
required fields and the `due` shape are pinned).
ACCEPTANCE: Help/version assertions pass natively; removing a required
field or renaming a schema property fails the manifest test.
FILES: `tests/cli.rs`.

### 24. Extend the time-parser fuzz corpus
SPEC: Add multibyte-soup inputs (multibyte tails, combining marks,
empty/whitespace strings) and absolute-stamp range edges (years 0001
and 10000, month 13, day 32) to the deterministic 10k-case fuzz loop,
asserting `Invalid` vs `OutOfRange` per items 6 and 8 with no panics.
ACCEPTANCE: Fuzz loop passes with the new corpus; reverting item 6 or 8
fails it.
FILES: `src/time.rs`.

## Batch E — Build, CI, and repo health

### 25. Constrain the published crate file list and rename the PRD
SPEC: Add `include = ["src/**", "tests/**", "examples/**",
"scripts/**", "README.md", "CHANGELOG.md", "LICENSE-*", "deny.toml",
"rust-toolchain.toml"]` to `Cargo.toml`; `git mv` the unicode/space
PRD filename to `docs/PRD.md`; remove the stale `target/package` copy
via build cleanup.
ACCEPTANCE: `cargo package --list` shows only listed paths with no
unicode/space names; `cargo package --verify` passes; repo-wide grep
for the old PRD name is clean.
FILES: `Cargo.toml`, `docs/PRD.md`.

### 26. Lock credential hygiene in with an automated guard
SPEC: Add `tests/secrets.rs`: it asserts the live `.crates.env` token
file is git-ignored (`git check-ignore`) and untracked (`git ls-files`),
and proves both guard polarities against throwaway `git init` fixture
repos (a tracked secret fails, an ignored secret passes). The file
itself is never read, so the test cannot echo the key. Deleting the
live file and rotating the key remain human steps on crates.io; this
item makes an accidental commit impossible to land silently.
ACCEPTANCE: `cargo test --test secrets` passes; the fixture cases fail
the guard for a tracked secret and for an un-ignored secret.
FILES: `tests/secrets.rs`.

### 27. Fix the Makefile: test/component split, clean, header
SPEC: Run `test` as `env -u DEKOPON_PROVIDER_COMPONENT cargo test
--locked` (unset, not empty: the suites skip only when the var is
unset); drop the stale `pledge-core.wasm` name from `clean`; fix the
`make check` header to "fmt + clippy + deny + machete + e2e (the CI
gate)".
ACCEPTANCE: `make build && make test` prints the skip notices and never
runs component suites; `make clean` leaves no leftovers; header matches
AGENTS.md in substance.
FILES: `Makefile`.

### 28. Consolidate the CI gate on make check with cached tools
SPEC: Replace the Format..Test step block with a single `run: make
check` (keeping setup, daemon checks, and artifact upload); cache
`~/.cargo/bin` plus the registry keyed by tool versions so the four
`cargo install` provisioning steps (deny, machete, both daemons) are not
rebuilt from source every run.
ACCEPTANCE: CI log shows one `make check`; a second consecutive run
skips compilation; bumping a tool version re-installs once; breaking a
Makefile gate fails CI.
FILES: `.github/workflows/ci.yml`.

### 29. Harden CI supply chain and runtime
SPEC: Pin `checkout@v4`, `dtolnay/rust-toolchain@1.98.1`, and
`upload-artifact@v4` to full commit SHAs with trailing `# vX.Y.Z`
comments (adopt dependabot/renovate for bumps); add top-level
`permissions: {contents: read}`, `concurrency: {group:
ci-${{ github.ref }}, cancel-in-progress: true}`, and
`timeout-minutes: 30` on the job.
ACCEPTANCE: No mutable action refs remain; CI green on pinned SHAs;
re-push cancels the superseded run; token is read-scoped.
FILES: `.github/workflows/ci.yml`.

### 30. Strict and portable component build script
SPEC: In `scripts/build-component.sh`, exit 1 naming expected vs actual
versions on a wasm-tools mismatch unless
`ALLOW_WASM_TOOLS_MISMATCH=1` is set; probe `command -v sha256sum ||
shasum -a 256` for the sidecar step and error clearly if neither
exists.
ACCEPTANCE: Build with a mismatched wasm-tools exits 1 with both
versions named (override builds with the warning); build completes on a
PATH with only `shasum`, sidecar bytes identical.
FILES: `scripts/build-component.sh`.
