//! Manifest and command-grammar tests: no component build required.
//!
//! These assert the provider's static contract — six capabilities with their
//! ids, effects, risks and closed schemas — and that every argv maps to the
//! right proposal without touching any import.

use dekopon_provider_pledge::Pledge;
use dekopon_provider_sdk::{CommandRunOutcome, EffectKind, RiskLevel, provider};
use serde_json::json;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn manifest_lists_six_closed_capabilities() {
    let manifest = provider::manifest::<Pledge>().expect("closed schemas publish");
    assert_eq!(manifest.id.as_str(), "pledge");
    assert_eq!(manifest.command_words, ["pledge"]);
    let capabilities = &manifest.capabilities;
    assert_eq!(capabilities.len(), 6);
    for (capability, name, effect, risk) in [
        (
            &capabilities[0],
            "make",
            EffectKind::LocalWrite,
            RiskLevel::Low,
        ),
        (
            &capabilities[1],
            "list",
            EffectKind::ReadOnly,
            RiskLevel::Low,
        ),
        (
            &capabilities[2],
            "due",
            EffectKind::ReadOnly,
            RiskLevel::Low,
        ),
        (
            &capabilities[3],
            "keep",
            EffectKind::LocalWrite,
            RiskLevel::Low,
        ),
        (
            &capabilities[4],
            "drop",
            EffectKind::LocalWrite,
            RiskLevel::Low,
        ),
        (
            &capabilities[5],
            "stats",
            EffectKind::ReadOnly,
            RiskLevel::Low,
        ),
    ] {
        assert_eq!(capability.id.as_str(), format!("pledge.{name}"));
        assert_eq!(capability.effect, effect);
        assert_eq!(capability.risk, risk);
        assert!(!capability.description.is_empty());
        assert_eq!(
            capability.input_schema["additionalProperties"],
            json!(false),
            "{name} schema is closed"
        );
        assert_eq!(capability.input_schema["type"], json!("object"));
    }
    let schema = &capabilities[0].input_schema;
    assert_eq!(schema["required"], json!(["to", "due"]));
    assert_eq!(schema["properties"]["to"]["maxLength"], json!(64));
    let due = &capabilities[2].input_schema;
    assert!(
        due.get("required")
            .is_none_or(|required| required == &json!([]))
    );
    assert!(due["properties"]["fail_if_none"].is_object());
}

#[test]
fn every_subcommand_proposes_its_capability() {
    for (words, piped, id, input) in [
        (
            argv(&[
                "make",
                "--to",
                "alice",
                "--due",
                "2h",
                "Confirm",
                "migration",
            ]),
            false,
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "Confirm migration"}),
        ),
        (
            argv(&["make", "--to", "bob", "--due", "2026-10-14T16:00:00-04:00"]),
            true,
            "pledge.make",
            json!({"to": "bob", "due": "2026-10-14T16:00:00-04:00"}),
        ),
        (
            argv(&["list", "--to", "alice", "--open", "--limit", "5"]),
            false,
            "pledge.list",
            json!({"to": "alice", "state": "open", "limit": 5, "oldest_first": false, "fail_if_none": false}),
        ),
        (
            argv(&["list"]),
            false,
            "pledge.list",
            json!({"oldest_first": false, "fail_if_none": false}),
        ),
        (
            argv(&["list", "--settled", "--oldest-first", "--fail-if-none"]),
            false,
            "pledge.list",
            json!({"state": "settled", "oldest_first": true, "fail_if_none": true}),
        ),
        (
            argv(&["due", "--within", "1h", "--fail-if-none"]),
            false,
            "pledge.due",
            json!({"within": "1h", "fail_if_none": true}),
        ),
        (
            argv(&["due", "--to", "alice", "--limit", "3"]),
            false,
            "pledge.due",
            json!({"to": "alice", "limit": 3, "fail_if_none": false}),
        ),
        (
            argv(&["due"]),
            false,
            "pledge.due",
            json!({"fail_if_none": false}),
        ),
        (
            argv(&["keep", "p-7f3a9c", "--note", "done", "quickly"]),
            false,
            "pledge.keep",
            json!({"id": "p-7f3a9c", "note": "done quickly"}),
        ),
        (
            argv(&["drop", "p-7f3a9c", "--reason", "no", "longer", "needed"]),
            false,
            "pledge.drop",
            json!({"id": "p-7f3a9c", "reason": "no longer needed"}),
        ),
        (
            argv(&["make", "--to", "a", "--due", "2020-01-01T00:00:00Z", "late"]),
            false,
            "pledge.make",
            json!({"to": "a", "due": "2020-01-01T00:00:00Z", "text": "late"}),
        ),
        (
            argv(&["stats", "--to", "alice"]),
            false,
            "pledge.stats",
            json!({"to": "alice"}),
        ),
    ] {
        match provider::command::<Pledge>(&words, piped) {
            CommandRunOutcome::Proposed {
                capability,
                input: proposed,
                ..
            } => {
                assert_eq!(capability.as_str(), id, "{words:?}");
                assert_eq!(proposed, input, "{words:?}");
            }
            outcome => panic!("{words:?} must propose, got {outcome:?}"),
        }
    }
}

#[test]
fn invalid_argv_fails_pure_without_an_import() {
    // Usage refusals from propose: no import touched, model sees the reason.
    for (words, piped) in [
        (argv(&["make", "--to", "alice", "--due", "2h"]), false),
        (argv(&["make", "--to", "", "--due", "2h", "text"]), false),
        (
            argv(&["make", "--to", "alice", "--due", "tomorrow", "text"]),
            false,
        ),
        (argv(&["list", "--limit", "0"]), false),
        (argv(&["due", "--within", "tomorrow"]), false),
        (argv(&["due", "--limit", "0"]), false),
        (argv(&["drop", "p-7f3a9c", "--reason", "  "]), false),
    ] {
        match provider::command::<Pledge>(&words, piped) {
            CommandRunOutcome::Failed { error } => {
                assert_eq!(error.code, "usage", "{words:?}");
                assert!(!error.message.is_empty());
            }
            outcome => panic!("{words:?} must fail usage, got {outcome:?}"),
        }
    }
    // Clap-level refusals render help or usage errors with nonzero status.
    for words in [
        argv(&["list", "--open", "--settled"]),
        argv(&["drop", "p-7f3a9c"]),
        argv(&["make", "--to", "alice"]),
        argv(&["bogus"]),
        argv(&["--definitely-invalid-option"]),
    ] {
        match provider::command::<Pledge>(&words, false) {
            CommandRunOutcome::Rendered {
                stdout,
                stderr,
                status,
            } => {
                assert_ne!(status, 0, "{words:?}");
                assert!(stdout.is_empty(), "{words:?}");
                assert!(!stderr.is_empty(), "{words:?}");
                assert!(!stderr.contains('\u{1b}'), "{words:?}");
            }
            outcome => panic!("{words:?} must render, got {outcome:?}"),
        }
    }
    match provider::command::<Pledge>(&argv(&["--help"]), false) {
        CommandRunOutcome::Rendered {
            stdout,
            stderr,
            status,
        } => {
            assert_eq!(status, 0);
            assert!(stderr.is_empty());
            assert!(stdout.contains("pledge"));
            assert!(!stdout.contains('\u{1b}'));
        }
        outcome => panic!("--help must render, got {outcome:?}"),
    }
}
