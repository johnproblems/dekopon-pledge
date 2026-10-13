//! Pledge: a durable ledger of the promises a Dekopon agent makes.
//!
//! Six capabilities under provider id `pledge` and command word `pledge`.
//! Writes append one event to a broker-owned JSONL stream; reads fold the
//! stream into promises. Time comes from the broker wall clock and ids from
//! broker entropy, never from input.

pub mod cli;
pub mod error;
pub mod input;
pub mod model;
pub mod store;
pub mod time;

use std::io::{Read, Write};

use dekopon_provider_sdk::provider::{
    Capability, Clock, Jsonl, Proposal, Provider, Random, Stdin, Stdout, Storage, Usage,
    jsonl::StorageError, stdin,
};
use dekopon_provider_sdk::{EffectKind, RiskLevel, export};

use crate::cli::{Args, Cmd};
use crate::error::{PledgeError, storage};
use crate::input::{DropInput, DueInput, KeepInput, ListInput, MakeInput, StateFilter, StatsInput};
use crate::model::{
    Promise, Settled, dropped_line, fold, kept_line, made_line, promise_value,
    recipient_stats_value, stats, stats_by_recipient, stats_value, valid_id,
};
use crate::store::{PledgeStore, read_ledger};
use crate::time::MAX_MILLIS;

/// Promises listed when `--limit` is absent.
pub const DEFAULT_LIST_LIMIT: u32 = 100;
/// `due` window when `--within` is absent: 24 hours.
pub const DEFAULT_DUE_WINDOW_MS: u64 = 24 * 3_600_000;
/// Piped stdin is rejected above this many bytes (promise text caps at 280).
const MAX_STDIN_BYTES: u64 = 4_096;

/// The pledge provider.
pub struct Pledge;

/// Records a promise owed to someone, due at a time.
pub struct Make;
/// Lists promises, newest first.
pub struct List;
/// Shows open promises due within a window, plus overdue ones.
pub struct Due;
/// Marks a promise kept.
pub struct Keep;
/// Marks a promise dropped, with a reason.
pub struct Drop;
/// Reports follow-through counts and the on-time rate.
pub struct Stats;

impl Provider for Pledge {
    const ID: &'static str = "pledge";
    const COMMAND_WORDS: &'static [&'static str] = &["pledge"];
    const DESCRIPTION: &'static str =
        "A ledger of promises this agent made, and whether it kept them.";
    type Args = Args;
    type Capabilities = (Make, List, Due, Keep, Drop, Stats);

    fn propose(args: Args, stdin_piped: bool) -> Result<Proposal<Self>, Usage> {
        match args.cmd {
            Cmd::Make { .. } => Ok(Proposal::to::<Make>(args.cmd.make_input(stdin_piped)?)),
            Cmd::List { .. } => Ok(Proposal::to::<List>(args.cmd.list_input()?)),
            Cmd::Due { .. } => Ok(Proposal::to::<Due>(args.cmd.due_input()?)),
            Cmd::Keep { .. } => Ok(Proposal::to::<Keep>(args.cmd.keep_input()?)),
            Cmd::Drop { .. } => Ok(Proposal::to::<Drop>(args.cmd.drop_input()?)),
            Cmd::Stats { .. } => Ok(Proposal::to::<Stats>(args.cmd.stats_input()?)),
        }
    }
}

/// Reads one validated write through a size race: re-folds and re-checks when
/// the host reports the ledger moved under the append.
fn transact(
    store: &impl PledgeStore,
    mut attempt: impl FnMut(&[Promise]) -> Result<Vec<u8>, PledgeError>,
) -> Result<(), PledgeError> {
    for _ in 0..2 {
        let (size, bytes) = read_ledger(store)?;
        let promises = fold(&bytes)?;
        let record = attempt(&promises)?;
        match store.append(crate::store::LEDGER, size, &record) {
            Ok(_) => return Ok(()),
            Err(StorageError::Busy) => continue,
            Err(error) => return Err(storage(error)),
        }
    }
    Err(PledgeError::StorageBusy)
}

/// Mints `p-` plus six hex characters from broker entropy.
fn mint_id(rng: &Random) -> String {
    let mut bytes = [0_u8; 3];
    rng.fill(&mut bytes);
    format!("p-{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2])
}

/// Reads piped promise text, enforcing the 280-byte bound and UTF-8.
fn read_stdin_text() -> Result<String, PledgeError> {
    let Some(input) = stdin() else {
        return Err(PledgeError::EmptyText);
    };
    read_piped_text(input)
}

fn read_piped_text(input: Stdin) -> Result<String, PledgeError> {
    let mut bytes = Vec::new();
    input
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PledgeError::StdinFailed)?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return Err(PledgeError::TextTooLong);
    }
    let text = String::from_utf8(bytes).map_err(|_| PledgeError::StdinFailed)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(PledgeError::EmptyText);
    }
    if trimmed.len() > model::MAX_TEXT_BYTES {
        return Err(PledgeError::TextTooLong);
    }
    Ok(trimmed.to_owned())
}

/// Writes one compact JSON object line.
fn write_line(out: &mut impl Write, value: &serde_json::Value) -> Result<(), PledgeError> {
    serde_json::to_writer(&mut *out, value).map_err(|_| PledgeError::OutputClosed)?;
    out.write_all(b"\n").map_err(|_| PledgeError::OutputClosed)
}

fn recipient_matches(promise: &Promise, to: Option<&str>) -> bool {
    to.is_none_or(|want| promise.to.to_lowercase() == want.to_lowercase())
}

/// Refuses a broker clock reading past the representable range before it can
/// be stamped into an event. Reported as a host clock failure, never as input
/// or ledger corruption.
fn check_clock(now_ms: u64) -> Result<(), PledgeError> {
    if now_ms > MAX_MILLIS {
        return Err(PledgeError::ClockOutOfRange);
    }
    Ok(())
}

impl Capability for Make {
    type Provider = Pledge;
    const NAME: &'static str = "make";
    const DESCRIPTION: &'static str = "Record a promise owed to someone, due at a time.";
    const EFFECT: EffectKind = EffectKind::LocalWrite;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = MakeInput;
    type Needs = (Storage<Jsonl>, Clock, Random);
    type Error = PledgeError;

    fn run(
        input: MakeInput,
        (store, clock, rng): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        let now_ms = clock.now_unix_millis();
        check_clock(now_ms)?;
        let due_ms = input.due.resolve(now_ms).map_err(|_| PledgeError::BadDue)?;
        let to = input.to.as_str().trim();
        if to.is_empty() {
            return Err(PledgeError::EmptyRecipient);
        }
        let text = match &input.text {
            Some(text) => {
                let trimmed = text.as_str().trim();
                if trimmed.is_empty() {
                    return Err(PledgeError::EmptyText);
                }
                trimmed.to_owned()
            }
            None => read_stdin_text()?,
        };
        let mut made = None;
        transact(&store, |promises| {
            let mut id = mint_id(&rng);
            for _ in 0..8 {
                if promises.iter().all(|promise| promise.id != id) {
                    break;
                }
                id = mint_id(&rng);
            }
            if promises.iter().any(|promise| promise.id == id) {
                return Err(PledgeError::StorageFailed);
            }
            made = Some(Promise {
                id: id.clone(),
                to: to.to_owned(),
                due_ms,
                text: text.clone(),
                made_at_ms: now_ms,
                settled: None,
            });
            made_line(&id, now_ms, to, due_ms, &text)
        })?;
        let Some(promise) = made else {
            return Err(PledgeError::StorageFailed);
        };
        write_line(out, &promise_value(&promise, now_ms)?)
    }
}

impl Capability for List {
    type Provider = Pledge;
    const NAME: &'static str = "list";
    const DESCRIPTION: &'static str = "List promises, newest first, as JSON lines.";
    const EFFECT: EffectKind = EffectKind::ReadOnly;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = ListInput;
    type Needs = (Storage<Jsonl>, Clock);
    type Error = PledgeError;

    fn run(
        input: ListInput,
        (store, clock): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        let limit = input.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 {
            return Err(PledgeError::BadLimit);
        }
        let now_ms = clock.now_unix_millis();
        let (_, bytes) = read_ledger(&store)?;
        let promises = fold(&bytes)?;
        let mut selected: Vec<(usize, &Promise)> = promises
            .iter()
            .enumerate()
            .filter(|(_, promise)| {
                recipient_matches(promise, input.to.as_ref().map(|to| to.as_str()))
                    && match input.state {
                        Some(StateFilter::Open) => promise.settled.is_none(),
                        Some(StateFilter::Settled) => promise.settled.is_some(),
                        Some(StateFilter::Kept) => {
                            matches!(promise.settled, Some(Settled::Kept { .. }))
                        }
                        Some(StateFilter::Dropped) => {
                            matches!(promise.settled, Some(Settled::Dropped { .. }))
                        }
                        None => true,
                    }
                    && (!input.overdue_only || promise.is_overdue(now_ms))
            })
            .collect();
        if input.oldest_first {
            selected.sort_by(|left, right| {
                left.1
                    .made_at_ms
                    .cmp(&right.1.made_at_ms)
                    .then_with(|| left.0.cmp(&right.0))
            });
        } else {
            selected.sort_by(|left, right| {
                right
                    .1
                    .made_at_ms
                    .cmp(&left.1.made_at_ms)
                    .then_with(|| right.0.cmp(&left.0))
            });
        }
        if selected.is_empty() && input.fail_if_none {
            return Err(PledgeError::NoneFound);
        }
        for (_, promise) in selected.iter().take(limit as usize) {
            write_line(out, &promise_value(promise, now_ms)?)?;
        }
        Ok(())
    }
}

impl Capability for Due {
    type Provider = Pledge;
    const NAME: &'static str = "due";
    const DESCRIPTION: &'static str = "Show open promises due within a window, plus overdue ones.";
    const EFFECT: EffectKind = EffectKind::ReadOnly;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = DueInput;
    type Needs = (Storage<Jsonl>, Clock);
    type Error = PledgeError;

    fn run(
        input: DueInput,
        (store, clock): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        let within_ms = input
            .within
            .as_ref()
            .map(|within| within.resolve())
            .transpose()
            .map_err(|_| PledgeError::BadWindow)?
            .unwrap_or(DEFAULT_DUE_WINDOW_MS);
        let limit = input.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 {
            return Err(PledgeError::BadLimit);
        }
        let now_ms = clock.now_unix_millis();
        let cutoff = now_ms.saturating_add(within_ms);
        let (_, bytes) = read_ledger(&store)?;
        let promises = fold(&bytes)?;
        let mut selected: Vec<(usize, &Promise)> = promises
            .iter()
            .enumerate()
            .filter(|(_, promise)| {
                promise.settled.is_none()
                    && promise.due_ms <= cutoff
                    && recipient_matches(promise, input.to.as_ref().map(|to| to.as_str()))
            })
            .collect();
        selected.sort_by(|left, right| {
            left.1
                .due_ms
                .cmp(&right.1.due_ms)
                .then_with(|| left.1.made_at_ms.cmp(&right.1.made_at_ms))
                .then_with(|| left.0.cmp(&right.0))
        });
        if selected.is_empty() && input.fail_if_none {
            return Err(PledgeError::NoneDue);
        }
        for (_, promise) in selected.iter().take(limit as usize) {
            write_line(out, &promise_value(promise, now_ms)?)?;
        }
        Ok(())
    }
}

impl Capability for Keep {
    type Provider = Pledge;
    const NAME: &'static str = "keep";
    const DESCRIPTION: &'static str = "Mark a promise kept, with an optional note.";
    const EFFECT: EffectKind = EffectKind::LocalWrite;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = KeepInput;
    type Needs = (Storage<Jsonl>, Clock);
    type Error = PledgeError;

    fn run(
        input: KeepInput,
        (store, clock): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        if !valid_id(&input.id) {
            return Err(PledgeError::BadId);
        }
        let now_ms = clock.now_unix_millis();
        check_clock(now_ms)?;
        let note = input
            .note
            .as_ref()
            .map(|note| note.as_str().trim())
            .filter(|note| !note.is_empty());
        let mut settled = None;
        transact(&store, |promises| {
            let found = promises
                .iter()
                .find(|promise| promise.id == input.id && promise.settled.is_none())
                .ok_or(PledgeError::NotOpen)?;
            let mut promise = found.clone();
            promise.settled = Some(Settled::Kept {
                at_ms: now_ms,
                note: note.map(str::to_owned),
            });
            settled = Some(promise);
            kept_line(&input.id, now_ms, note)
        })?;
        let Some(promise) = settled else {
            return Err(PledgeError::StorageFailed);
        };
        write_line(out, &promise_value(&promise, now_ms)?)
    }
}

impl Capability for Drop {
    type Provider = Pledge;
    const NAME: &'static str = "drop";
    const DESCRIPTION: &'static str = "Mark a promise dropped, with a reason.";
    const EFFECT: EffectKind = EffectKind::LocalWrite;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = DropInput;
    type Needs = (Storage<Jsonl>, Clock);
    type Error = PledgeError;

    fn run(
        input: DropInput,
        (store, clock): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        if !valid_id(&input.id) {
            return Err(PledgeError::BadId);
        }
        let now_ms = clock.now_unix_millis();
        check_clock(now_ms)?;
        let reason = input.reason.as_str().trim();
        if reason.is_empty() {
            return Err(PledgeError::EmptyReason);
        }
        let mut settled = None;
        transact(&store, |promises| {
            let found = promises
                .iter()
                .find(|promise| promise.id == input.id && promise.settled.is_none())
                .ok_or(PledgeError::NotOpen)?;
            let mut promise = found.clone();
            promise.settled = Some(Settled::Dropped {
                at_ms: now_ms,
                reason: reason.to_owned(),
            });
            settled = Some(promise);
            dropped_line(&input.id, now_ms, reason)
        })?;
        let Some(promise) = settled else {
            return Err(PledgeError::StorageFailed);
        };
        write_line(out, &promise_value(&promise, now_ms)?)
    }
}

impl Capability for Stats {
    type Provider = Pledge;
    const NAME: &'static str = "stats";
    const DESCRIPTION: &'static str =
        "Report kept, broken and open counts with the follow-through rate.";
    const EFFECT: EffectKind = EffectKind::ReadOnly;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = StatsInput;
    type Needs = (Storage<Jsonl>, Clock);
    type Error = PledgeError;

    fn run(
        input: StatsInput,
        (store, clock): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), PledgeError> {
        let now_ms = clock.now_unix_millis();
        let (_, bytes) = read_ledger(&store)?;
        let promises = fold(&bytes)?;
        let selected: Vec<Promise> = promises
            .iter()
            .filter(|promise| recipient_matches(promise, input.to.as_ref().map(|to| to.as_str())))
            .cloned()
            .collect();
        if input.by_recipient {
            for (to, counts) in stats_by_recipient(&selected, now_ms) {
                write_line(out, &recipient_stats_value(&to, &counts))?;
            }
            return Ok(());
        }
        write_line(out, &stats_value(&stats(&selected, now_ms)))
    }
}

export!(Pledge);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FakeStore;

    #[test]
    fn transact_rechecks_after_a_busy_append() {
        let store = FakeStore::new();
        let record = made_line_for_test("p-000001");
        store.busy_appends.set(1);
        let mut attempts = 0;
        transact(&store, |_| {
            attempts += 1;
            Ok(record.clone())
        })
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(fold(&store.data.borrow()).unwrap().len(), 1);
    }

    #[test]
    fn transact_returns_decisive_errors_without_writing() {
        let store = FakeStore::new();
        let error = transact(&store, |_| Err(PledgeError::NotOpen)).unwrap_err();
        assert_eq!(error, PledgeError::NotOpen);
        assert!(store.data.borrow().is_empty());
    }

    #[test]
    fn clock_past_year_9999_is_its_own_failure() {
        assert_eq!(check_clock(MAX_MILLIS), Ok(()));
        assert_eq!(
            check_clock(MAX_MILLIS + 1),
            Err(PledgeError::ClockOutOfRange)
        );
    }

    #[test]
    fn recipient_matching_ignores_case() {
        let promise = Promise {
            id: "p-000001".to_owned(),
            to: "Alice".to_owned(),
            due_ms: 0,
            text: "t".to_owned(),
            made_at_ms: 0,
            settled: None,
        };
        assert!(recipient_matches(&promise, None));
        assert!(recipient_matches(&promise, Some("alice")));
        assert!(recipient_matches(&promise, Some("ALICE")));
        assert!(!recipient_matches(&promise, Some("bob")));
    }

    fn made_line_for_test(id: &str) -> Vec<u8> {
        crate::model::made_line(
            id,
            1_790_985_600_000,
            "alice",
            1_790_985_600_000,
            "do the thing",
        )
        .unwrap()
    }

    #[test]
    fn transact_reports_storage_busy_after_two_busy_appends() {
        let store = FakeStore::new();
        store.busy_appends.set(2);
        let error = transact(&store, |_| Ok(made_line_for_test("p-000001"))).unwrap_err();
        assert_eq!(error, PledgeError::StorageBusy);
        assert!(store.data.borrow().is_empty());
    }

    #[test]
    fn transact_denial_maps_to_storage_failed_and_writes_nothing() {
        let mut store = FakeStore::new();
        store.deny_append = true;
        let error = transact(&store, |_| Ok(made_line_for_test("p-000001"))).unwrap_err();
        assert_eq!(error, PledgeError::StorageFailed);
        assert!(store.data.borrow().is_empty());
    }
}
