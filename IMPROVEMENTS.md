# Pledge improvements (0.2.0 batch)

Twenty-five scoped, verifiable improvements. No new capabilities, no
scheduler, no compact/snooze (those stay stretch goals). Each item ships with
its test or check.

## Capability & UX

- [x] 1. `s` and `w` duration units (`30s`, `2w`) for `--due`/`--within`.
- [x] 2. `pledge due --limit N` (default 100) — probe-safe bounded output.
- [x] 3. `pledge due --to <who>` recipient filter.
- [x] 4. `pledge list --oldest-first` ordering flag.
- [x] 5. `--note`/`--reason` accept multiple words without quotes.
- [x] 6. `pledge list --fail-if-none` with new `none-found` code (exit 1).
- [x] 7. Past-due `make` pinned: allowed, echoes `"overdue":true`.

## Error taxonomy & robustness

- [x] 8. `ledger-too-large` code (exit 1) for the 16 MiB read bound.
- [x] 9. `clock-out-of-range` code (exit 1) for a clock past year 9999.
- [x] 10. Multibyte boundary + newline/quote/unicode round-trip tests.
- [x] 11. Corrupt-ledger e2e: planted garbage line → `ledger-corrupt`, exit 1.

## Verification depth

- [x] 12. Stdin e2e: wire stdin in the harness; text/empty/overlong/non-UTF8.
- [x] 13. `run-command` e2e through the real component (propose-in-wasm).
- [x] 14. `due` edge e2e: empty ledger ± fail flag; `--limit`/`--to` in-wasm.
- [x] 15. Deterministic time-parser fuzz loop (10k cases, no new deps).
- [x] 16. Id-format + uniqueness e2e: 20 real makes, `^p-[0-9a-f]{6}$`.

## Build / CI / ops

- [x] 17. CI runs both daemons' `check` on `examples/broker`.
- [x] 18. `cargo-deny` config + CI step.
- [x] 19. `cargo-machete` CI step.
- [x] 20. `make install PROVIDERS_DIR=...` (+ `uninstall`).
- [x] 21. Build script writes a `pledge.wasm.sha256` sidecar.

## Docs & release

- [x] 22. README jq cookbook.
- [x] 23. README "Semantics" section (canonical time, overdue, orderings).
- [x] 24. README exact `wake watch` payload for the probe pairing.
- [x] 25. Release 0.2.0: version bump + CHANGELOG.
