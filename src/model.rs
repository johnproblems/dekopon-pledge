//! The ledger data model: append-only events folded into promises.
//!
//! The stream holds one JSON object per line: a `made` event followed by at
//! most one `kept` or `dropped` event per id. Current state is a fold over
//! those events, so no record is ever rewritten. Anything that does not fit
//! the schema — unknown versions, unknown events, inconsistent transitions,
//! out-of-bound text — is [`PledgeError::LedgerCorrupt`], never silently
//! skipped: a ledger that drops lines loses promises.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::PledgeError;
use crate::time::{format_rfc3339, parse_rfc3339};

/// Schema version written by this provider; anything else is corrupt.
const SCHEMA_VERSION: u8 = 1;

/// Maximum recipient length in UTF-8 bytes.
pub const MAX_TO_BYTES: usize = 64;
/// Maximum promise text length in UTF-8 bytes.
pub const MAX_TEXT_BYTES: usize = 280;
/// Maximum note and reason length in UTF-8 bytes.
pub const MAX_NOTE_BYTES: usize = 200;

#[derive(Debug, Deserialize)]
#[serde(tag = "ev", deny_unknown_fields)]
enum StoredEvent {
    #[serde(rename = "made")]
    Made {
        v: u8,
        id: String,
        at: String,
        to: String,
        due: String,
        text: String,
    },
    #[serde(rename = "kept")]
    Kept {
        v: u8,
        id: String,
        at: String,
        note: Option<String>,
    },
    #[serde(rename = "dropped")]
    Dropped {
        v: u8,
        id: String,
        at: String,
        reason: String,
    },
}

/// How a promise was settled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Settled {
    /// Finished, with the settlement time and optional note.
    Kept { at_ms: u64, note: Option<String> },
    /// Withdrawn, with the settlement time and required reason.
    Dropped { at_ms: u64, reason: String },
}

/// One promise: its terms plus its settlement, if any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Promise {
    /// `p-` plus six hex characters.
    pub id: String,
    /// Who the promise is owed to.
    pub to: String,
    /// Absolute due instant in Unix milliseconds.
    pub due_ms: u64,
    /// What was promised.
    pub text: String,
    /// When the promise was made, in Unix milliseconds.
    pub made_at_ms: u64,
    /// How it ended, or `None` while open.
    pub settled: Option<Settled>,
}

impl Promise {
    /// Whether the promise is open and past its due time.
    #[must_use]
    pub fn is_overdue(&self, now_ms: u64) -> bool {
        self.settled.is_none() && self.due_ms < now_ms
    }

    /// Whether the promise was kept strictly after its due time.
    #[must_use]
    pub fn was_kept_late(&self) -> bool {
        matches!(&self.settled, Some(Settled::Kept { at_ms, .. }) if *at_ms > self.due_ms)
    }
}

/// Folds the raw ledger bytes into promises in ledger order (oldest first).
pub fn fold(bytes: &[u8]) -> Result<Vec<Promise>, PledgeError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(PledgeError::LedgerCorrupt);
    }
    let mut promises: Vec<Promise> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        let event: StoredEvent =
            serde_json::from_slice(line).map_err(|_| PledgeError::LedgerCorrupt)?;
        apply(event, &mut promises, &mut index)?;
    }
    Ok(promises)
}

/// Whether an id has the minted shape: `p-` plus six lowercase hex characters.
pub(crate) fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 8
        && bytes[0] == b'p'
        && bytes[1] == b'-'
        && bytes[2..]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn apply(
    event: StoredEvent,
    promises: &mut Vec<Promise>,
    index: &mut HashMap<String, usize>,
) -> Result<(), PledgeError> {
    let corrupt = PledgeError::LedgerCorrupt;
    let version = match &event {
        StoredEvent::Made { v, .. }
        | StoredEvent::Kept { v, .. }
        | StoredEvent::Dropped { v, .. } => *v,
    };
    if version != SCHEMA_VERSION {
        return Err(corrupt);
    }
    match event {
        StoredEvent::Made {
            id,
            at,
            to,
            due,
            text,
            ..
        } => {
            if !valid_id(&id) || to.trim().is_empty() || text.trim().is_empty() {
                return Err(corrupt);
            }
            if to.len() > MAX_TO_BYTES || text.len() > MAX_TEXT_BYTES {
                return Err(corrupt);
            }
            if index.contains_key(id.as_str()) {
                return Err(corrupt);
            }
            let made_at_ms = parse_rfc3339(&at).map_err(|_| corrupt)?;
            let due_ms = parse_rfc3339(&due).map_err(|_| corrupt)?;
            index.insert(id.clone(), promises.len());
            promises.push(Promise {
                id,
                to,
                due_ms,
                text,
                made_at_ms,
                settled: None,
            });
            Ok(())
        }
        StoredEvent::Kept { id, at, note, .. } => {
            let slot = *index.get(id.as_str()).ok_or(corrupt)?;
            if promises[slot].settled.is_some() {
                return Err(corrupt);
            }
            if note
                .as_ref()
                .is_some_and(|note| note.trim().is_empty() || note.len() > MAX_NOTE_BYTES)
            {
                return Err(corrupt);
            }
            let at_ms = parse_rfc3339(&at).map_err(|_| corrupt)?;
            if at_ms < promises[slot].made_at_ms {
                return Err(corrupt);
            }
            promises[slot].settled = Some(Settled::Kept { at_ms, note });
            Ok(())
        }
        StoredEvent::Dropped { id, at, reason, .. } => {
            let slot = *index.get(id.as_str()).ok_or(corrupt)?;
            if promises[slot].settled.is_some() {
                return Err(corrupt);
            }
            if reason.trim().is_empty() || reason.len() > MAX_NOTE_BYTES {
                return Err(corrupt);
            }
            let at_ms = parse_rfc3339(&at).map_err(|_| corrupt)?;
            if at_ms < promises[slot].made_at_ms {
                return Err(corrupt);
            }
            promises[slot].settled = Some(Settled::Dropped { at_ms, reason });
            Ok(())
        }
    }
}

/// Follow-through counts over a set of promises at `now_ms`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// Open promises, including overdue ones.
    pub open: u64,
    /// Promises marked kept, on time or late.
    pub kept: u64,
    /// Promises marked dropped.
    pub dropped: u64,
    /// Open promises past their due time.
    pub broken: u64,
    /// Promises kept strictly after their due time.
    pub kept_late: u64,
    /// `kept / (kept + broken)`, or `None` when no promise was kept or broken.
    pub on_time_rate: Option<f64>,
}

/// Computes [`Stats`]; `broken` is derived, never stored.
#[must_use]
pub fn stats(promises: &[Promise], now_ms: u64) -> Stats {
    let mut counts = Stats {
        open: 0,
        kept: 0,
        dropped: 0,
        broken: 0,
        kept_late: 0,
        on_time_rate: None,
    };
    for promise in promises {
        match &promise.settled {
            None => {
                counts.open += 1;
                if promise.is_overdue(now_ms) {
                    counts.broken += 1;
                }
            }
            Some(Settled::Kept { .. }) => {
                counts.kept += 1;
                if promise.was_kept_late() {
                    counts.kept_late += 1;
                }
            }
            Some(Settled::Dropped { .. }) => counts.dropped += 1,
        }
    }
    let denominator = counts.kept + counts.broken;
    if denominator > 0 {
        counts.on_time_rate = Some(counts.kept as f64 / denominator as f64);
    }
    counts
}

/// Renders one promise as the JSON object the read capabilities emit.
pub fn promise_value(promise: &Promise, now_ms: u64) -> Result<Value, PledgeError> {
    let mut value = json!({
        "id": promise.id,
        "to": promise.to,
        "due": format_rfc3339(promise.due_ms).map_err(|_| PledgeError::LedgerCorrupt)?,
        "text": promise.text,
        "made_at": format_rfc3339(promise.made_at_ms).map_err(|_| PledgeError::LedgerCorrupt)?,
        "overdue": promise.is_overdue(now_ms),
        "late": promise.was_kept_late(),
    });
    let (state, settled_at, extra) = match &promise.settled {
        None => ("open", None, None),
        Some(Settled::Kept { at_ms, note }) => (
            "kept",
            Some(*at_ms),
            note.as_ref().map(|note| ("note", note.as_str())),
        ),
        Some(Settled::Dropped { at_ms, reason }) => {
            ("dropped", Some(*at_ms), Some(("reason", reason.as_str())))
        }
    };
    value["state"] = Value::String(state.to_owned());
    if let Some(at_ms) = settled_at {
        value["settled_at"] =
            Value::String(format_rfc3339(at_ms).map_err(|_| PledgeError::LedgerCorrupt)?);
    }
    if let Some((key, text)) = extra {
        value[key] = Value::String(text.to_owned());
    }
    Ok(value)
}

/// Renders [`Stats`] as the single object `pledge stats` emits.
#[must_use]
pub fn stats_value(counts: &Stats) -> Value {
    json!({
        "open": counts.open,
        "kept": counts.kept,
        "dropped": counts.dropped,
        "broken": counts.broken,
        "kept_late": counts.kept_late,
        "on_time_rate": counts.on_time_rate,
    })
}

/// Computes [`Stats`] per recipient over a set of promises at `now_ms`,
/// sorted by recipient. Recipients group by their stored spelling.
#[must_use]
pub fn stats_by_recipient(promises: &[Promise], now_ms: u64) -> Vec<(String, Stats)> {
    let mut groups: HashMap<&str, Vec<&Promise>> = HashMap::new();
    for promise in promises {
        groups.entry(promise.to.as_str()).or_default().push(promise);
    }
    let mut recipients: Vec<&str> = groups.keys().copied().collect();
    recipients.sort_unstable();
    recipients
        .iter()
        .map(|to| {
            let owned: Vec<Promise> = groups[to]
                .iter()
                .map(|promise| (*promise).clone())
                .collect();
            ((*to).to_owned(), stats(&owned, now_ms))
        })
        .collect()
}

/// Renders one recipient's [`Stats`] as a `pledge stats --by-recipient` line.
#[must_use]
pub fn recipient_stats_value(to: &str, counts: &Stats) -> Value {
    json!({
        "to": to,
        "open": counts.open,
        "kept": counts.kept,
        "dropped": counts.dropped,
        "broken": counts.broken,
        "kept_late": counts.kept_late,
        "on_time_rate": counts.on_time_rate,
    })
}

#[derive(Debug, Serialize)]
struct MadeEvent<'a> {
    v: u8,
    ev: &'static str,
    id: &'a str,
    at: String,
    to: &'a str,
    due: String,
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct KeptEvent<'a> {
    v: u8,
    ev: &'static str,
    id: &'a str,
    at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct DroppedEvent<'a> {
    v: u8,
    ev: &'static str,
    id: &'a str,
    at: String,
    reason: &'a str,
}

/// Serializes a `made` event line (without the trailing newline the host adds).
pub fn made_line(
    id: &str,
    at_ms: u64,
    to: &str,
    due_ms: u64,
    text: &str,
) -> Result<Vec<u8>, PledgeError> {
    let event = MadeEvent {
        v: SCHEMA_VERSION,
        ev: "made",
        id,
        at: format_rfc3339(at_ms).map_err(|_| PledgeError::ClockOutOfRange)?,
        to,
        due: format_rfc3339(due_ms).map_err(|_| PledgeError::ClockOutOfRange)?,
        text,
    };
    serde_json::to_vec(&event).map_err(|_| PledgeError::StorageFailed)
}

/// Serializes a `kept` event line (without the trailing newline the host adds).
pub fn kept_line(id: &str, at_ms: u64, note: Option<&str>) -> Result<Vec<u8>, PledgeError> {
    let event = KeptEvent {
        v: SCHEMA_VERSION,
        ev: "kept",
        id,
        at: format_rfc3339(at_ms).map_err(|_| PledgeError::ClockOutOfRange)?,
        note,
    };
    serde_json::to_vec(&event).map_err(|_| PledgeError::StorageFailed)
}

/// Serializes a `dropped` event line (without the trailing newline the host adds).
pub fn dropped_line(id: &str, at_ms: u64, reason: &str) -> Result<Vec<u8>, PledgeError> {
    let event = DroppedEvent {
        v: SCHEMA_VERSION,
        ev: "dropped",
        id,
        at: format_rfc3339(at_ms).map_err(|_| PledgeError::ClockOutOfRange)?,
        reason,
    };
    serde_json::to_vec(&event).map_err(|_| PledgeError::StorageFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MADE: &str = r#"{"v":1,"ev":"made","id":"p-7f3a9c","at":"2026-10-13T14:02:11.000Z","to":"alice","due":"2026-10-14T20:00:00.000Z","text":"Confirm migration finished"}"#;
    const KEPT: &str = r#"{"v":1,"ev":"kept","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","note":"migration done 15:42"}"#;

    fn ledger(lines: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        bytes
    }

    #[test]
    fn fold_tracks_open_and_settled_promises() {
        let promises = fold(&ledger(&[MADE])).unwrap();
        assert_eq!(promises.len(), 1);
        assert_eq!(promises[0].id, "p-7f3a9c");
        assert_eq!(promises[0].to, "alice");
        assert_eq!(promises[0].settled, None);

        let promises = fold(&ledger(&[MADE, KEPT])).unwrap();
        assert!(matches!(
            &promises[0].settled,
            Some(Settled::Kept { note: Some(note), .. }) if note == "migration done 15:42"
        ));

        let dropped = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","reason":"withdrawn"}"#;
        let promises = fold(&ledger(&[MADE, dropped])).unwrap();
        assert!(matches!(
            &promises[0].settled,
            Some(Settled::Dropped { reason, .. }) if reason == "withdrawn"
        ));

        assert!(fold(&[]).unwrap().is_empty());
    }

    #[test]
    fn fold_refuses_every_inconsistency() {
        let unknown = r#"{"v":1,"ev":"kept","id":"p-000000","at":"2026-10-14T19:42:05.000Z"}"#;
        let double = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","reason":"twice"}"#;
        let future = MADE.replace(r#""v":1"#, r#""v":2"#);
        let snoozed = MADE.replace(r#""ev":"made""#, r#""ev":"snoozed""#);
        let long = MADE.replace("alice", &"a".repeat(65));
        let empty_text = MADE.replace("Confirm migration finished", "");
        let bad_due = MADE.replace("2026-10-14T20:00:00.000Z", "soon");
        let extra = MADE.replace(r#""text":"#, r#""extra":1,"text":"#);
        for bytes in [
            ledger(&[MADE, unknown]),
            ledger(&[MADE, KEPT, double]),
            ledger(&[MADE, MADE]),
            ledger(&[&future]),
            ledger(&[&snoozed]),
            ledger(&[&long]),
            ledger(&[&empty_text]),
            ledger(&[&bad_due]),
            ledger(&[&extra]),
            b"not-json\n".to_vec(),
            MADE.as_bytes().to_vec(),
            b"\n".to_vec(),
        ] {
            assert_eq!(fold(&bytes), Err(PledgeError::LedgerCorrupt));
        }
    }

    #[test]
    fn fold_rejects_ids_outside_the_minted_shape() {
        let overlong = format!("p-{}", "a".repeat(70));
        for bad in [
            "P-7F3A9C",
            "p-7F3A9C",
            "p-xyz",
            "p-7f3a9c00",
            "p-12345",
            "p-g00000",
            "",
            overlong.as_str(),
        ] {
            let line = MADE.replace("p-7f3a9c", bad);
            assert_eq!(
                fold(&ledger(&[&line])),
                Err(PledgeError::LedgerCorrupt),
                "{bad}"
            );
        }
        assert_eq!(fold(&ledger(&[MADE])).unwrap().len(), 1);
    }

    #[test]
    fn fold_rejects_settlements_before_the_promise_was_made() {
        let early = KEPT.replace("2026-10-14T19:42:05.000Z", "2026-10-13T14:02:10.000Z");
        assert_eq!(
            fold(&ledger(&[MADE, &early])),
            Err(PledgeError::LedgerCorrupt)
        );
        let early_drop = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-13T14:02:10.999Z","reason":"withdrawn"}"#;
        assert_eq!(
            fold(&ledger(&[MADE, early_drop])),
            Err(PledgeError::LedgerCorrupt)
        );
        let same_ms = KEPT.replace("2026-10-14T19:42:05.000Z", "2026-10-13T14:02:11.000Z");
        assert_eq!(fold(&ledger(&[MADE, &same_ms])).unwrap().len(), 1);
    }

    #[test]
    fn fold_rejects_blank_after_trim_text_but_keeps_the_original() {
        let blank_to = MADE.replace("alice", "   ");
        let blank_text = MADE.replace("Confirm migration finished", "\\t");
        let blank_note = KEPT.replace("migration done 15:42", "  ");
        let blank_reason = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","reason":" \t "}"#;
        for bytes in [
            ledger(&[&blank_to]),
            ledger(&[&blank_text]),
            ledger(&[MADE, &blank_note]),
            ledger(&[MADE, blank_reason]),
        ] {
            assert_eq!(fold(&bytes), Err(PledgeError::LedgerCorrupt));
        }
        let padded = MADE.replace("alice", " alice ");
        let promises = fold(&ledger(&[&padded])).unwrap();
        assert_eq!(promises[0].to, " alice ");
    }

    #[test]
    fn broken_is_derived_and_rates_follow_through() {
        let now = parse_rfc3339("2026-10-15T00:00:00.000Z").unwrap();
        let promises = fold(&ledger(&[MADE])).unwrap();
        assert!(promises[0].is_overdue(now));
        assert!(!promises[0].is_overdue(promises[0].due_ms));
        let counts = stats(&promises, now);
        assert_eq!(
            counts,
            Stats {
                open: 1,
                kept: 0,
                dropped: 0,
                broken: 1,
                kept_late: 0,
                on_time_rate: Some(0.0),
            }
        );

        // Keep one of two, let the other go overdue: on_time_rate 0.5.
        let second = MADE.replace("p-7f3a9c", "p-000002");
        let promises = fold(&ledger(&[MADE, &second, KEPT])).unwrap();
        let counts = stats(&promises, now);
        assert_eq!(counts.on_time_rate, Some(0.5));
        assert_eq!(counts.kept_late, 0);

        // A late keep still counts as kept, and is flagged separately.
        let late = KEPT.replace("2026-10-14T19:42:05.000Z", "2026-10-15T00:00:01.000Z");
        let promises = fold(&ledger(&[MADE, &late])).unwrap();
        assert!(promises[0].was_kept_late());
        let counts = stats(&promises, now);
        assert_eq!(counts.on_time_rate, Some(1.0));
        assert_eq!(counts.kept_late, 1);

        // Drops do not punish the rate; an empty ledger reports null.
        let dropped = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-14T19:42:05.000Z","reason":"withdrawn"}"#;
        let promises = fold(&ledger(&[MADE, dropped])).unwrap();
        assert_eq!(stats(&promises, now).on_time_rate, None);
        assert_eq!(stats(&[], now).on_time_rate, None);
        assert_eq!(
            stats_value(&stats(&[], now)),
            json!({"open":0,"kept":0,"dropped":0,"broken":0,"kept_late":0,"on_time_rate":None::<f64>})
        );
    }

    #[test]
    fn written_events_fold_back_exactly() {
        let at = parse_rfc3339("2026-10-13T14:02:11.000Z").unwrap();
        let due = parse_rfc3339("2026-10-14T20:00:00.000Z").unwrap();
        let made = made_line("p-abc123", at, "alice", due, "do the thing").unwrap();
        let kept = kept_line("p-abc123", due, Some("done")).unwrap();
        assert!(!made.contains(&b'\n'));
        let lines: Vec<&str> = [
            std::str::from_utf8(&made).unwrap(),
            std::str::from_utf8(&kept).unwrap(),
        ]
        .into_iter()
        .collect();
        let promises = fold(&ledger(&lines)).unwrap();
        assert_eq!(promises[0].id, "p-abc123");
        assert!(matches!(&promises[0].settled, Some(Settled::Kept { .. })));

        let dropped = dropped_line("p-abc123", due, "withdrawn").unwrap();
        let lines: Vec<&str> = [
            std::str::from_utf8(&made).unwrap(),
            std::str::from_utf8(&dropped).unwrap(),
        ]
        .into_iter()
        .collect();
        let promises = fold(&ledger(&lines)).unwrap();
        assert!(matches!(
            &promises[0].settled,
            Some(Settled::Dropped { .. })
        ));
    }

    #[test]
    fn multibyte_text_round_trips_and_bounds_count_bytes() {
        let at = parse_rfc3339("2026-10-13T14:02:11.000Z").unwrap();
        let due = parse_rfc3339("2026-10-14T20:00:00.000Z").unwrap();
        let text = "line one\nline \"two\" ☃️ done";
        let made = made_line("p-abc123", at, "Zoë", due, text).unwrap();
        assert!(!made.contains(&b'\n'));
        let line = std::str::from_utf8(&made).unwrap();
        let promises = fold(&ledger(&[line])).unwrap();
        let value = promise_value(&promises[0], at).unwrap();
        assert_eq!(value["text"], text);
        assert_eq!(value["to"], "Zoë");

        // Bounds count UTF-8 bytes, not characters: 32 é fills the 64-byte
        // recipient bound and 140 é fills the 280-byte text bound exactly.
        let ok = made_line("p-abc124", at, &"é".repeat(32), due, &"é".repeat(140)).unwrap();
        let line = std::str::from_utf8(&ok).unwrap();
        assert_eq!(fold(&ledger(&[line])).unwrap().len(), 1);
        // 33 é is 66 bytes: over the 64-byte recipient bound.
        let bad_to = made_line("p-abc125", at, &"é".repeat(33), due, "t").unwrap();
        let line = std::str::from_utf8(&bad_to).unwrap();
        assert_eq!(fold(&ledger(&[line])), Err(PledgeError::LedgerCorrupt));
        // 141 é is 282 bytes: over the 280-byte text bound.
        let bad_text = made_line("p-abc126", at, "a", due, &"é".repeat(141)).unwrap();
        let line = std::str::from_utf8(&bad_text).unwrap();
        assert_eq!(fold(&ledger(&[line])), Err(PledgeError::LedgerCorrupt));
    }

    #[test]
    fn promise_output_flags_overdue_and_settlement() {
        let now = parse_rfc3339("2026-10-15T00:00:00.000Z").unwrap();
        let promises = fold(&ledger(&[MADE])).unwrap();
        let value = promise_value(&promises[0], now).unwrap();
        assert_eq!(value["state"], "open");
        assert_eq!(value["overdue"], true);
        assert!(value.get("settled_at").is_none());

        let promises = fold(&ledger(&[MADE, KEPT])).unwrap();
        let value = promise_value(&promises[0], now).unwrap();
        assert_eq!(value["state"], "kept");
        assert_eq!(value["overdue"], false);
        assert_eq!(value["settled_at"], "2026-10-14T19:42:05.000Z");
        assert_eq!(value["note"], "migration done 15:42");
    }

    #[test]
    fn promise_output_flags_kept_late_only() {
        let now = parse_rfc3339("2026-10-15T00:00:00.000Z").unwrap();
        let promises = fold(&ledger(&[MADE])).unwrap();
        assert_eq!(promise_value(&promises[0], now).unwrap()["late"], false);

        // Kept on time: false.
        let promises = fold(&ledger(&[MADE, KEPT])).unwrap();
        assert_eq!(promise_value(&promises[0], now).unwrap()["late"], false);

        // Kept strictly after the due time: true.
        let late = KEPT.replace("2026-10-14T19:42:05.000Z", "2026-10-15T00:00:01.000Z");
        let promises = fold(&ledger(&[MADE, &late])).unwrap();
        assert_eq!(promise_value(&promises[0], now).unwrap()["late"], true);

        // Dropped, even past due, is never late.
        let dropped = r#"{"v":1,"ev":"dropped","id":"p-7f3a9c","at":"2026-10-15T00:00:01.000Z","reason":"withdrawn"}"#;
        let promises = fold(&ledger(&[MADE, dropped])).unwrap();
        assert_eq!(promise_value(&promises[0], now).unwrap()["late"], false);
    }

    #[test]
    fn recipient_stats_group_sort_and_count() {
        let now = parse_rfc3339("2026-10-15T00:00:00.000Z").unwrap();
        let bob = MADE.replace("p-7f3a9c", "p-000002").replace("alice", "bob");
        let kept_bob = KEPT.replace("p-7f3a9c", "p-000002");
        let promises = fold(&ledger(&[MADE, &bob, &kept_bob])).unwrap();
        let grouped = stats_by_recipient(&promises, now);
        assert_eq!(
            grouped
                .iter()
                .map(|(to, _)| to.as_str())
                .collect::<Vec<_>>(),
            ["alice", "bob"]
        );
        assert_eq!(grouped[0].1.open, 1);
        assert_eq!(grouped[0].1.broken, 1);
        assert_eq!(grouped[1].1, stats(&promises[1..2], now));
        assert_eq!(
            recipient_stats_value(&grouped[1].0, &grouped[1].1),
            json!({"to":"bob","open":0,"kept":1,"dropped":0,"broken":0,"kept_late":0,"on_time_rate":1.0})
        );
        assert!(stats_by_recipient(&[], now).is_empty());
    }

    #[test]
    fn write_side_timestamp_failures_are_clock_out_of_range() {
        use crate::time::MAX_MILLIS;

        let at = parse_rfc3339("2026-10-13T14:02:11.000Z").unwrap();
        let past_end_of_time = MAX_MILLIS + 1;
        assert_eq!(
            made_line("p-abc123", at, "alice", past_end_of_time, "t"),
            Err(PledgeError::ClockOutOfRange)
        );
        assert_eq!(
            made_line("p-abc123", past_end_of_time, "alice", at, "t"),
            Err(PledgeError::ClockOutOfRange)
        );
        assert_eq!(
            kept_line("p-abc123", past_end_of_time, None),
            Err(PledgeError::ClockOutOfRange)
        );
        assert_eq!(
            dropped_line("p-abc123", past_end_of_time, "withdrawn"),
            Err(PledgeError::ClockOutOfRange)
        );
        // Data read back from the ledger is correctly corrupt, not a clock fault.
        let unfolded = Promise {
            id: "p-abc123".to_owned(),
            to: "alice".to_owned(),
            due_ms: past_end_of_time,
            text: "t".to_owned(),
            made_at_ms: at,
            settled: None,
        };
        assert_eq!(
            promise_value(&unfolded, at),
            Err(PledgeError::LedgerCorrupt)
        );
    }
}
