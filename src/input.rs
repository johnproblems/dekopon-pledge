//! Capability inputs: closed JSON schemas over bounded strings.
//!
//! Every struct carries `#[serde(deny_unknown_fields)]`, which the SDK's
//! manifest check requires: an open object schema refuses to publish. Time
//! specifications stay strings here and are resolved against the broker clock
//! in `run`, never in `propose`.

use std::borrow::Cow;

use dekopon_provider_sdk::provider::Bounded;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::model::{MAX_NOTE_BYTES, MAX_TEXT_BYTES, MAX_TO_BYTES};
use crate::time::{TimeError, parse_duration, resolve_due};

/// A `--due` value: RFC 3339 or a relative duration like `2h`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DueSpec(String);

impl DueSpec {
    /// Wraps raw input without validating; validation happens in `run`.
    #[must_use]
    pub fn new(spec: impl Into<String>) -> Self {
        Self(spec.into())
    }

    /// The raw specification.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the spec parses as either an absolute or relative time.
    pub fn check(&self) -> Result<(), TimeError> {
        resolve_due(&self.0, 0).map(|_| ())
    }

    /// Resolves the spec against the broker clock to Unix milliseconds.
    pub fn resolve(&self, now_ms: u64) -> Result<u64, TimeError> {
        resolve_due(&self.0, now_ms)
    }
}

impl Serialize for DueSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DueSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl JsonSchema for DueSpec {
    fn schema_name() -> Cow<'static, str> {
        "DueSpec".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "description": "RFC 3339 timestamp or relative duration like 30s, 30m, 2h, 3d or 2w"})
    }
}

/// A `--within` value: a relative duration like `1h`, never absolute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurationSpec(String);

impl DurationSpec {
    /// Wraps raw input without validating; validation happens in `run`.
    #[must_use]
    pub fn new(spec: impl Into<String>) -> Self {
        Self(spec.into())
    }

    /// The raw specification.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the spec parses as a relative duration.
    pub fn check(&self) -> Result<(), TimeError> {
        parse_duration(&self.0).map(|_| ())
    }

    /// Resolves the spec to a millisecond duration.
    pub fn resolve(&self) -> Result<u64, TimeError> {
        parse_duration(&self.0)
    }
}

impl Serialize for DurationSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl JsonSchema for DurationSpec {
    fn schema_name() -> Cow<'static, str> {
        "DurationSpec".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "description": "Relative duration like 30s, 30m, 2h, 3d or 2w"})
    }
}

/// A `list` settlement filter: `open`, `settled`, or absent for all.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StateFilter {
    /// Unsettled promises only.
    Open,
    /// Kept and dropped promises only.
    Settled,
}

/// Input for `pledge.make`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MakeInput {
    /// Who the promise is owed to, at most 64 UTF-8 bytes.
    pub to: Bounded<MAX_TO_BYTES>,
    /// When it is due: RFC 3339 or a relative duration.
    pub due: DueSpec,
    /// What was promised, at most 280 UTF-8 bytes; piped stdin when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Bounded<MAX_TEXT_BYTES>>,
}

/// Input for `pledge.list`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    /// Only promises owed to this recipient (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Bounded<MAX_TO_BYTES>>,
    /// Only open or only settled promises; all when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateFilter>,
    /// Maximum promises to emit; 100 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Oldest first instead of newest first.
    #[serde(default)]
    pub oldest_first: bool,
    /// Fail with `none-found` when nothing matches.
    #[serde(default)]
    pub fail_if_none: bool,
}

/// Input for `pledge.due`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DueInput {
    /// Window ahead of now; 24h when absent. Overdue promises always show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<DurationSpec>,
    /// Only promises owed to this recipient (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Bounded<MAX_TO_BYTES>>,
    /// Maximum promises to emit; 100 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Fail with `none-due` when nothing is due: the watch-probe idle signal.
    #[serde(default)]
    pub fail_if_none: bool,
}

/// Input for `pledge.keep`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepInput {
    /// The promise id, `p-` plus six hex characters.
    pub id: String,
    /// Optional note, at most 200 UTF-8 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Bounded<MAX_NOTE_BYTES>>,
}

/// Input for `pledge.drop`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DropInput {
    /// The promise id, `p-` plus six hex characters.
    pub id: String,
    /// Why the promise was dropped, at most 200 UTF-8 bytes.
    pub reason: Bounded<MAX_NOTE_BYTES>,
}

/// Input for `pledge.stats`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatsInput {
    /// Only promises owed to this recipient (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Bounded<MAX_TO_BYTES>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_fields_are_refused_and_optionals_default() {
        let make: MakeInput = serde_json::from_value(json!({"to": "alice", "due": "1h"})).unwrap();
        assert!(make.text.is_none());
        assert!(
            serde_json::from_value::<MakeInput>(json!({"to": "a", "due": "1h", "extra": 1}))
                .is_err()
        );
        assert!(serde_json::from_value::<MakeInput>(json!({"to": "a"})).is_err());

        let list: ListInput = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            (list.to.is_none(), list.state, list.limit),
            (true, None, None)
        );
        let list: ListInput =
            serde_json::from_value(json!({"state": "settled", "limit": 5})).unwrap();
        assert_eq!(
            (list.state, list.limit),
            (Some(StateFilter::Settled), Some(5))
        );
        assert!(serde_json::from_value::<ListInput>(json!({"state": "all"})).is_err());

        assert!(serde_json::from_value::<KeepInput>(json!({"id": "p-000000"})).is_ok());
        assert!(serde_json::from_value::<DropInput>(json!({"id": "p-000000"})).is_err());
    }

    #[test]
    fn bounds_reject_overlong_text() {
        let long = "x".repeat(MAX_TO_BYTES + 1);
        assert!(serde_json::from_value::<MakeInput>(json!({"to": long, "due": "1h"})).is_err());
        let long = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(
            serde_json::from_value::<MakeInput>(json!({"to": "a", "due": "1h", "text": long}))
                .is_err()
        );
        let long = "x".repeat(MAX_NOTE_BYTES + 1);
        assert!(
            serde_json::from_value::<DropInput>(json!({"id": "p-000000", "reason": long})).is_err()
        );
    }

    #[test]
    fn time_specs_validate_without_a_clock() {
        assert!(DueSpec::new("2026-10-14T20:00:00Z").check().is_ok());
        assert!(DueSpec::new("2h").check().is_ok());
        assert!(DueSpec::new("tomorrow").check().is_err());
        assert!(DurationSpec::new("1h").check().is_ok());
        assert!(DurationSpec::new("2026-10-14T20:00:00Z").check().is_err());
    }
}
