# PRD: Pledge — a commitments ledger provider for Dekopon agents

Oct 13, 2026 · @Ian Jones

## Summary

Pledge is a Dekopon WebAssembly provider, built on `dekopon-provider-sdk` 0.46.1, that gives chat agents a durable ledger of the promises they make — "I'll check the deploy at 3pm", "remind Sam about the invoice Friday" — and lets them answer "what do I still owe?" on demand. Target build: one person, 8–24 hours, one `.wasm` component plus tests and an example deployment.

**The gap.** Dekopon's README lists task memory among the things that [do not exist yet](https://github.com/dekopon-agents/dekopon): chat memory is on-demand turn history, not commitments. An agent that says "I'll follow up" today has nowhere to put that promise, so it is lost the moment the conversation scrolls. The shipped providers (echo, JSONPlaceholder, gh, turso-sql, memory-chat) cover I/O and recall, not obligations.

**Why a provider, not a feature.** The SDK already supplies everything Pledge needs: broker-owned JSONL storage scoped per namespace, an authorized broker wall clock, closed JSON-schema inputs, and per-capability effect and risk labels the broker's Cedar policy can gate. Pledge is pure composition of those primitives — no new authority, no network, no secrets.

## Goals, non-goals, success criteria

The build succeeds when an agent in `dekopon-run shell` can make, list, and settle a promise, and a broker restart loses none of them.

**Goals**

- Six capabilities (below) exported from one component, each with a closed input schema and an honest effect/risk label.
- Promises survive broker restarts via broker-owned JSONL storage.
- "What's due?" is answered from the broker clock, never a model-supplied time.
- Output is JSON lines, so `jq` in `dekopon-shell` can filter and count.

**Non-goals (v0.1)**

- No proactive nudging. Dekopon has no scheduler that wakes an agent; Pledge only answers when asked.
- No cross-agent or cross-sender sharing — storage scope is whatever the broker assigns.
- No natural-language date parsing; inputs are RFC 3339 timestamps or relative durations like `2h`.
- No deletion UX beyond marking a promise kept, dropped or broken.

**Success criteria**

| Criterion | Target |
| --- | --- |
| Capabilities exported and listed by `dekopon-run inspect` | 6 of 6 |
| Testkit end-to-end tests passing (make → list → keep) | 100% |
| Schemas accepted by the SDK's closed-schema check | 0 `ManifestError`s |
| Component size (release, `wasm32`) | under 1 MiB |
| Restart survival in the broker example | all open promises listed after restart |

## Users and core scenarios

The primary user is a self-hosting operator running a Dekopon agent in Slack, Discord or Telegram; the model is the one calling the capabilities.

1. **Make.** A teammate says "ping me when the migration finishes, should be around 4". The agent runs `pledge make --to alice --due 2026-10-14T16:00:00-04:00 "Confirm migration finished"` and replies with the promise id.
2. **Recall.** Next morning someone asks "anything you owe me?". The agent runs `pledge list --to alice --open` and answers from the ledger, not from scrolled-off chat.
3. **Due check.** At the start of any turn, an operator's standing prompt can tell the agent to run `pledge due --within 1h` and mention anything overdue.
4. **Settle.** The agent finishes the work and runs `pledge keep p-7f3a --note "migration done 15:42"`. Dropping a promise the requester withdrew uses `pledge drop`.
5. **Honesty report.** `pledge stats` returns kept / broken / open counts so an operator can see how reliable the agent's promises actually are.

Scenario 5 is the novel part: overdue promises that were never settled are counted as broken, giving agents a measurable follow-through rate.

## Capabilities and command grammar

Pledge exports six capabilities under provider id `pledge` and one command word, `pledge`; writes are `LocalWrite`, reads are `ReadOnly`, and all are `Low` risk because nothing leaves the broker.

| Capability id | Command | Effect | Risk | Imports (`Needs`) | Output |
| --- | --- | --- | --- | --- | --- |
| `pledge.make` | `pledge make --to <who> --due <time> <text>` | LocalWrite | Low | Jsonl, Clock, Random | the new promise as one JSON line |
| `pledge.list` | `pledge list [--to <who>] [--open\|--settled] [--limit N]` | ReadOnly | Low | Jsonl, Clock | one JSON line per promise, newest first |
| `pledge.due` | `pledge due [--within <dur>]` | ReadOnly | Low | Jsonl, Clock | open promises due within the window or overdue |
| `pledge.keep` | `pledge keep <id> [--note <text>]` | LocalWrite | Low | Jsonl, Clock | the settled promise |
| `pledge.drop` | `pledge drop <id> --reason <text>` | LocalWrite | Low | Jsonl, Clock | the settled promise |
| `pledge.stats` | `pledge stats [--to <who>]` | ReadOnly | Low | Jsonl, Clock | `{open, kept, dropped, broken, on_time_rate}` |

**Grammar rules**

- `--due` accepts RFC 3339 (`2026-10-14T16:00:00-04:00`) or a relative duration (`30m`, `2h`, `3d`). Relative values are resolved in `run` against the broker clock, never in `propose`, because `propose` is pure and has no imports.
- Promise text is capped at 280 UTF-8 bytes using the SDK's `Bounded<280>`; notes and reasons at 200.
- Piped stdin is accepted for `make` as the promise text, so `echo "..." | pledge make --to bob --due 1d` works in `dekopon-shell`.
- `broken` is derived, not stored: an open promise past its due time counts as broken in `stats` and is flagged `"overdue":true` in `list` and `due`.

## Data model and storage

The ledger is an append-only event log in one broker-owned JSONL stream; current state is a fold over events, so no record is ever rewritten.

That choice fits the SDK: its JSONL module applies writes per host call [without invocation-wide rollback](https://docs.rs/dekopon-provider-sdk/0.46.1/dekopon_provider_sdk/provider/index.html), so a single append per write capability is the only atomic unit Pledge relies on.

```json
{"v":1,"ev":"made","id":"p-7f3a9c","at":"2026-10-13T14:02:11Z","to":"alice","due":"2026-10-14T20:00:00Z","text":"Confirm migration finished"}
{"v":1,"ev":"kept","id":"p-7f3a9c","at":"2026-10-14T19:42:05Z","note":"migration done 15:42"}
```

| Field | Type | Notes |
| --- | --- | --- |
| `v` | integer | schema version, starts at 1 |
| `ev` | `made` \| `kept` \| `dropped` | event kind |
| `id` | string | `p-` + 6 hex chars from the broker `Random` import |
| `at` | RFC 3339, UTC | from the broker `Clock`, never from input |
| `to` | string, ≤ 64 bytes | who the promise is owed to; free text the agent supplies |
| `due` | RFC 3339, UTC | resolved absolute time |
| `text` / `note` / `reason` | bounded string | 280 / 200 / 200 bytes |

**Rules**

- A `kept` or `dropped` event for an unknown id, or one already settled, fails with code `not-open` (exit 1) and writes nothing.
- Reads fold the whole stream; at v0.1 scale (hundreds of promises) that is fine. Compaction is a stretch goal.
- Storage scope (per agent, sender or channel) is set by the broker's constraint catalog, not by Pledge; the provider never sees identity.

## Technical design against dekopon-provider-sdk 0.46.1

One crate, `dekopon-provider-pledge`, compiled to a `wasm32` component; each capability is a zero-sized type implementing the SDK's `Capability` trait, listed in a tuple on one `Provider`.

**How a call flows.** The model writes a `dekopon-shell` line; the shell routes the `pledge` command word to the component's argv grammar; `Provider::propose` maps clap-parsed args to a `Proposal` that [grants nothing](https://docs.rs/dekopon-provider-sdk/0.46.1/dekopon_provider_sdk/provider/index.html); the broker authorizes it under Cedar; only then does `Capability::run` execute with its granted imports and write to `Stdout`.

**Sketch** (trait shapes are from the [0.46.1 source](https://docs.rs/dekopon-provider-sdk/0.46.1/src/dekopon_provider_sdk/provider.rs.html); handle method names and the `export!` invocation must be checked against their docs):

```rust
use dekopon_provider_sdk::provider::{Bounded, Capability, Clock, Code, Failure, Jsonl, Proposal, Provider, Random, Stdout, Usage};
use dekopon_provider_sdk::{EffectKind, RiskLevel, clap, schemars};

pub struct Pledge;

impl Provider for Pledge {
    const ID: &'static str = "pledge";
    const COMMAND_WORDS: &'static [&'static str] = &["pledge"];
    const DESCRIPTION: &'static str = "A ledger of promises this agent made, and whether it kept them.";
    type Args = cli::Args;            // clap::Parser with make/list/due/keep/drop/stats subcommands
    type Capabilities = (Make, List, Due, Keep, Drop, Stats);

    fn propose(args: cli::Args, stdin_piped: bool) -> Result<Proposal<Self>, Usage> {
        match args.cmd {
            cli::Cmd::Make(m) => Ok(Proposal::to::<Make>(m.into_input(stdin_piped)?)),
            cli::Cmd::List(l) => Ok(Proposal::to::<List>(l.into())),
            // ...one arm per subcommand; pure, no imports
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]          // required: the SDK refuses open object schemas
pub struct MakeInput { to: Bounded<64>, due: DueSpec, text: Option<Bounded<280>> }

pub struct Make;
impl Capability for Make {
    type Provider = Pledge;
    const NAME: &'static str = "make";
    const DESCRIPTION: &'static str = "Record a promise owed to someone, due at a time.";
    const EFFECT: EffectKind = EffectKind::LocalWrite;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = MakeInput;
    type Needs = (Jsonl, Clock, Random);
    type Error = PledgeError;
    fn run(input: MakeInput, (log, clock, rng): Self::Needs, out: &mut Stdout) -> Result<(), PledgeError> {
        // resolve due against clock, mint id from rng, append one `made` event, echo it as JSON
        todo!()
    }
}

pub enum PledgeError { NotOpen, BadDue, EmptyText, Storage }
impl Failure for PledgeError {
    fn code(&self) -> Code { match self {
        Self::NotOpen => Code::new("not-open"),
        Self::BadDue => Code::new("bad-due").exiting(2),
        Self::EmptyText => Code::USAGE,
        Self::Storage => Code::new("storage-failed"),
    }}
}
```

**Design constraints the SDK enforces**

- Every input struct needs `#[serde(deny_unknown_fields)]` and no recursive types, or `manifest()` fails with `SchemaFault::Open` / `Reference`.
- `Code` names are kebab-case and checked at compile time; a zero exit status is a compile error.
- `Needs` is sealed: a capability only gets the imports its type declares, so `List` cannot write even by mistake if it declares `Jsonl` read paths only (verify whether the SDK splits read and write JSONL handles).
- Up to 32 capabilities per provider; Pledge uses 6.

## Security and policy posture

Pledge holds no secrets, makes no network calls and never sees who is asking; its whole blast radius is one JSONL namespace the broker assigns.

- **No HTTP, Assets, Spawn or Settings imports.** The component's import set is only JSONL, Clock and Random, which `dekopon-run inspect` and the broker web UI will show.
- **Time is trusted, input is not.** `at` and resolved `due` come from the broker clock. A model cannot backdate a `kept` event to improve its on-time rate.
- **Write separation.** Operators can grant read capabilities (`list`, `due`, `stats`) to every sender and reserve `make`/`keep`/`drop` for mapped senders through Cedar, since each is its own capability id.
- **Prompt-injection surface.** Promise text is model- or user-supplied and is echoed back on reads. It is data: Pledge never interprets it, and outputs are JSON-encoded so text cannot break the shell's stream framing.
- **Error messages never echo input**, matching the SDK's own `SdkFailure` convention.

## Build plan

The 8-hour floor delivers a working provider with all six capabilities; hours 8–18 make it trustworthy, and 18–24 are optional.

&#91;embedded content: build plan · 6 phases, 2 gates, 0–24h\]

If the build stops at hour 8, the result is still shippable through the testkit; the broker demo is what turns it into something an operator can run in chat.

## Testing and acceptance

Four automated layers plus a manual check, all offline; the end-to-end layer uses `dekopon-provider-sdk-testkit`, the in-process fake broker that runs a component against real storage.

| Layer | Tool | What it proves |
| --- | --- | --- |
| Unit | `cargo test` (native) | due parsing, event fold, `broken` derivation, id format, `Code` exit statuses |
| Manifest | `manifest::<Pledge>()` in a test | all 6 schemas are closed and inline; ids derive as `pledge.*` |
| Grammar | `command::<Pledge>(argv, piped)` | each subcommand yields the right `Proposal`; bad args yield `Rendered` help or a `usage` failure |
| End to end | testkit fake broker | make → list → keep → stats round trip; settling twice fails `not-open`; state persists across a new component instance |
| Manual | `dekopon-run shell` with the built `.wasm` | `pledge make ... \| jq -r .id` then `pledge keep $id` works in one script |

**Acceptance**

- [ ] `dekopon-run inspect --provider pledge.wasm` lists 6 capabilities with the effects and risks above
- [ ] All test layers green in CI
- [ ] A scripted demo: make two promises, keep one, let one go overdue, `pledge stats` reports `on_time_rate: 0.5`
- [ ] README with a Cedar snippet granting reads to all senders and writes to one

## Risks, open questions, stretch goals

The biggest risk is SDK churn: the crate is at 0.46.1, published 2026-10-13, while the repo README still describes application 0.12.0, so pin `=0.46.1` exactly.

| Risk | Impact | Mitigation |
| --- | --- | --- |
| SDK API shifts mid-build | rework of trait impls | pin `=0.46.1`; copy the echo provider's layout as a known-good template |
| `Jsonl` handle API differs from the sketch | hours lost on storage | read the `jsonl` module docs and a storage-backed provider (memory-chat) before hour 4 |
| Broker clock not available in direct `dekopon-run` mode | can't demo without a broker | demo through the testkit, then the `conditional-write` broker example |
| Models misuse `--due` | wrong deadlines | accept only RFC 3339 or `Nm/Nh/Nd`; reject everything else with `bad-due` |

**Open questions**

- Does the JSONL import expose separate read and append handles, so `ReadOnly` capabilities can be denied append at the type level?
- What namespace scope do operators want by default: per agent, per sender, or per channel?
- Is `on_time_rate` worth exposing to the model, or only to operators?

**Stretch goals (if time remains under 24h)**

- `pledge compact`: fold settled events into a snapshot file via `DurableFiles`.
- `pledge snooze <id> --by 1d` with a `snoozed` event, counted separately in stats.
- A standing-prompt snippet for `dekopond` routes so agents check `pledge due` on each turn.

## Sources

- [dekopon-provider-sdk 0.46.1 on docs.rs](https://docs.rs/dekopon-provider-sdk/0.46.1/dekopon_provider_sdk/)
- [provider.rs source, 0.46.1](https://docs.rs/dekopon-provider-sdk/0.46.1/src/dekopon_provider_sdk/provider.rs.html)
- [provider module index](https://docs.rs/dekopon-provider-sdk/0.46.1/dekopon_provider_sdk/provider/index.html)
- [Dekopon repository README](https://github.com/dekopon-agents/dekopon)
