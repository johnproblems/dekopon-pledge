# Pledge — a commitments ledger provider for Dekopon agents

Pledge is a Dekopon WebAssembly provider that gives chat agents a durable
ledger of the promises they make — "I'll check the deploy at 3pm", "remind
Sam about the invoice Friday" — and lets them answer "what do I still owe?"
on demand. One component, six capabilities, no network, no secrets: promises
survive broker restarts in broker-owned JSONL storage.

Built on `dekopon-provider-sdk =0.46.1` with Rust 1.98.1.

## Capabilities

Provider id `pledge`, command word `pledge`. Writes are `LocalWrite`, reads
are `ReadOnly`, all `Low` risk — nothing leaves the broker.

| Capability | Command | Effect |
|---|---|---|
| `pledge.make` | `pledge make --to <who> --due <time> <text>` | LocalWrite |
| `pledge.list` | `pledge list [--to <who>] [--open\|--settled] [--limit N] [--oldest-first] [--fail-if-none]` | ReadOnly |
| `pledge.due` | `pledge due [--within <dur>] [--to <who>] [--limit N] [--fail-if-none]` | ReadOnly |
| `pledge.keep` | `pledge keep <id> [--note <text>]` | LocalWrite |
| `pledge.drop` | `pledge drop <id> --reason <text>` | LocalWrite |
| `pledge.stats` | `pledge stats [--to <who>]` | ReadOnly |

`--due` accepts RFC 3339 (`2026-10-14T16:00:00-04:00`) or a relative duration
(`30s`, `30m`, `2h`, `3d`, `2w`, either case). Relative values resolve in
`run` against the broker wall clock — `propose` is pure and has no imports,
and the host traps clock reads outside an authorized `run`. Promise text caps
at 280 UTF-8 bytes; notes and reasons at 200; recipients at 64. Piped stdin
works as the promise text: `echo "..." | pledge make --to bob --due 1d`.
Multi-word `--note` and `--reason` need no quotes.

`pledge due --fail-if-none` exits 0 while at least one promise is due or
overdue within the window, and fails with code `none-due` (exit 1) while the
ledger is quiet. It stays `ReadOnly`: it exists so a watch probe can run it.

## Output

Exactly one compact JSON object per line, each with a trailing newline, so
`jq` can filter and count. A settled promise echoes back in full:

```json
{"id":"p-7f3a9c","to":"alice","due":"2026-10-14T20:00:00.000Z","text":"Confirm migration finished","made_at":"2026-10-13T14:02:11.000Z","overdue":false,"state":"kept","settled_at":"2026-10-14T19:42:05.000Z","note":"migration done 15:42"}
```

`pledge stats` prints one object with the follow-through rate:

```json
{"open":1,"kept":1,"dropped":0,"broken":1,"kept_late":0,"on_time_rate":0.5}
```

`broken` is derived, not stored: an open promise past its due time counts as
broken and is flagged `"overdue":true` in `list` and `due`. The rate is
`kept / (kept + broken)` — drops don't punish it — and `null` when nothing
was kept or broken yet. `kept_late` counts promises kept after their due
time; they still count as kept.

## Data model

The ledger is an append-only event log in one broker-owned JSONL stream;
current state is a fold over events, so no record is ever rewritten:

```json
{"v":1,"ev":"made","id":"p-7f3a9c","at":"2026-10-13T14:02:11.000Z","to":"alice","due":"2026-10-14T20:00:00.000Z","text":"Confirm migration finished"}
{"v":1,"ev":"kept","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","note":"migration done 15:42"}
```

Every write is a single conditional append — the only atomic unit the host
guarantees — retried once on a size race with a full re-fold and re-check, so
a raced settle can never corrupt the log. A `kept` or `dropped` event for an
unknown or already-settled id fails with `not-open` and writes nothing. Any
line that doesn't parse, or any inconsistent transition, fails reads as
`ledger-corrupt` rather than silently dropping promises. Ids are `p-` plus
six hex characters from broker entropy.

## Semantics

The rules every caller can rely on, each pinned by a test:

- **Time is canonical millis UTC.** Stored and emitted timestamps always look
  like `2026-10-14T20:00:00.000Z`: UTC, millisecond precision, fixed width,
  so lexicographic order is chronological order. Inputs accept offsets
  (`-04:00`) and fractional seconds (truncated to millis).
- **Overdue is strict.** A promise is overdue while open and its due instant
  is strictly before now. `pledge due --within 0m` therefore shows exactly
  the overdue set.
- **Past-due makes are allowed.** Recording an already-overdue promise
  succeeds and echoes back with `"overdue":true`.
- **Defaults are bounded.** `list` and `due` emit at most 100 promises unless
  `--limit` says otherwise; `due` looks 24h ahead unless `--within` says
  otherwise. Overdue promises always show in `due`, whatever the window.
- **Orderings are total.** `list` is newest-first (`--oldest-first` flips
  it); `due` is due-ascending, so the most overdue prints first. Ties break
  by ledger position, never randomly.
- **Recipients match case-insensitively** but are stored as given.
- **Settling is single-shot.** The second settle of an id fails `not-open`
  and writes nothing; a late keep still counts as kept (and is flagged
  `kept_late`), while drops never punish the rate.

## jq cookbook

```console
# How many open promises, and which are overdue?
pledge list --open | jq -s '{open: length, overdue: map(select(.overdue)) | length}'

# The single most overdue promise, as `id to due text`.
pledge due --within 0m | head -1 | jq -r '"\(.id) \(.to) \(.due) \(.text)"'

# Open promises per recipient.
pledge list --open | jq -s 'group_by(.to) | map({to: .[0].to, open: length})'

# Follow-through rate, defaulting an empty ledger to 1.
pledge stats | jq '.on_time_rate // 1'

# Watch-probe one-liner: exit 0 while anything is due or overdue.
pledge due --within 0m --fail-if-none
```

## Failure codes

| Code | Exit | Meaning |
|---|---|---|
| `not-open` | 1 | Settling an unknown or already-settled id |
| `bad-due` | 2 | `--due`/`--within` is not RFC 3339 or a duration |
| `usage` | 2 | Missing text, recipient, reason, or non-positive limit |
| `text-too-long` | 2 | Piped text over 280 UTF-8 bytes |
| `none-due` | 1 | `due --fail-if-none` found nothing due |
| `none-found` | 1 | `list --fail-if-none` found nothing matching |
| `stdin-failed` | 1 | Piped input could not be read as text |
| `ledger-corrupt` | 1 | A ledger line doesn't parse or the fold is inconsistent |
| `ledger-too-large` | 1 | The ledger exceeds the 16 MiB read bound |
| `clock-out-of-range` | 1 | The broker clock reads past year 9999 |
| `storage-failed` | 1 | Any host storage failure |
| `output-closed` | 1 | Stdout closed mid-write |

Messages are static and never echo input.

## Security and policy posture

Pledge holds no secrets, makes no network calls, and never sees who is
asking; its whole blast radius is one JSONL namespace the broker assigns. Its
import set is JSONL, Clock, and Random only — no HTTP, Assets, Spawn, or
Settings. A model cannot backdate events: `at` and resolved `due` come from
the broker clock. Promise text is echoed back as JSON-encoded data, never
interpreted.

Storage authority is set by the broker, per capability. Grant the reads
read-only JSONL and the writes read-write JSONL; the host rejects any write
attempted under a read-only grant, and the guest maps the denial to
`storage-failed` without writing. Scope is `private-conversation` (the
default below), `shared-conversation`, or `agent`:

```yaml
capabilities:
  pledge:
    constraints:
      timeoutMs: 10000
    capabilities:
      pledge.make:   { constraints: { storage: { interface: jsonl, access: read-write, scope: private-conversation } } }
      pledge.list:   { constraints: { storage: { interface: jsonl, access: read-only,  scope: private-conversation } } }
      pledge.due:    { constraints: { storage: { interface: jsonl, access: read-only,  scope: private-conversation } } }
      pledge.keep:   { constraints: { storage: { interface: jsonl, access: read-write, scope: private-conversation } } }
      pledge.drop:   { constraints: { storage: { interface: jsonl, access: read-write, scope: private-conversation } } }
      pledge.stats:  { constraints: { storage: { interface: jsonl, access: read-only,  scope: private-conversation } } }
```

Only listed capabilities run. In Cedar, use the built-in action groups to
grant reads to everyone and writes to named principals (every statement needs
an `@id`):

```cedar
@id("everyone-may-read-pledges")
permit(principal,
       action in Dekopon::Action::"pledge:read-only",
       resource == Dekopon::Provider::"pledge")
when { context has agent && context.agent == "pledge-keeper"
    && context has via && context.via == "dekopon-gatewayd" };

@id("owner-may-write-pledges")
permit(principal == Dekopon::Principal::"owner",
       action in [Dekopon::Action::"pledge.make",
                  Dekopon::Action::"pledge.keep",
                  Dekopon::Action::"pledge.drop"],
       resource == Dekopon::Provider::"pledge")
when { context has agent && context.agent == "pledge-keeper"
    && context has via && context.via == "dekopon-gatewayd" };
```

## Wakes

Dekopon's alarm clock is called wakes: a gateway tool offered to the model,
in two forms — come back after a delay, or come back when a watch probe (a
shell script run without a model) exits 0. Probes run under trigger `probe`
and may only use read-only capabilities; the broker refuses writes there.

Pledge runs no scheduler, daemon, timer, or polling loop, and a provider
cannot call the wake tool. The intended pairing is:

1. The model records the promise: `pledge make --to ... --due ... ...`.
2. Then it uses its `wake` tool: action `schedule` with `afterSeconds` and
   the promise id in the note, or action `watch` with the probe
   `pledge due --within 0m --fail-if-none` — exit 0 wakes the agent with
   what's due, exit 1 keeps waiting, and any usage failure (exit 2) wakes it
   with the misconfigured probe.

The exact watch call (every 5 minutes, for up to a day):

```json
{"action": "watch", "note": "Check due promises and follow up.", "script": "pledge due --within 0m --fail-if-none", "everySeconds": 300, "forSeconds": 86400}
```

Notes: the route needs `wakes: true` and the gateway needs `sessions.wakes`
configured (see `examples/broker/`); `$PREV` holds the previous probe run's
output, and the first run only seeds it, so the first wake always comes from
the second run onward.

## Build, test, deploy

Prerequisites: `rustup` (the pinned 1.98.1 toolchain installs itself via
`rust-toolchain.toml`) and `wasm-tools` 1.259.0.

```console
make build   # compile pledge.wasm (refuses at 1 MiB)
make test    # unit + manifest + grammar tests, no component needed
make e2e     # full suite: real component against real storage
make check   # fmt + clippy + deny + machete + full suite (the CI gate)
```

Test layers, all offline:

| Layer | Location | What it proves |
|---|---|---|
| Unit | `src/*/tests` | Due parsing, event fold, `broken` derivation, id minting, stable codes |
| Manifest | `tests/cli.rs` | `manifest::<Pledge>()` lists all 6 ids, effects, risks; schemas closed |
| Grammar | `tests/cli.rs` | Each argv proposes the right capability; bad argv fails pure |
| Conformance | `tests/component.rs` | Testkit: the `.wasm` matches its typed declarations and imports |
| End to end | `tests/ledger.rs` | Real component + real storage: make → list → keep → stats, double-settle `not-open`, read-only host rejection, restart survival |

[`examples/broker/`](examples/broker/) is a runnable deployment — broker and
gateway configs, Cedar policy, and an agent catalog with pledge standing
orders — served over an owner-only local socket. Its README walks through
`dekopon-brokerd check`, `dekopon-gatewayd check`, booting both daemons, and
talking to the agent.

## Layout

```text
src/
  lib.rs     provider, six capabilities, write transactions
  cli.rs     clap grammar and pure proposal inputs
  input.rs   closed input schemas
  model.rs   events, fold, stats, JSON views
  store.rs   JSONL access behind a testable trait
  time.rs    RFC 3339 and duration parsing without chrono
  error.rs   stable failure codes
tests/
  cli.rs       manifest + grammar (no build needed)
  component.rs testkit conformance (needs pledge.wasm)
  ledger.rs    storage end-to-end incl. restart (needs pledge.wasm)
examples/broker/  runnable broker+gateway deployment
scripts/build-component.sh
```

## License

MIT OR Apache-2.0.
