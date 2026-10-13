//! End-to-end ledger tests: the real component against real on-disk storage.
//!
//! The testkit's `Harness` cannot wire storage (it authorizes every call with
//! `storage: None`), so these tests drive
//! [`BrokerProviderRegistry::load_with_storage`] directly: each call is
//! authorized, granted a JSONL namespace, and invoked with a fixed broker
//! clock, exactly as a broker would. Restart survival drops the registry and
//! reopens the same storage root.
//!
//! Requires `DEKOPON_PROVIDER_COMPONENT` (see `scripts/build-component.sh`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dekopon_broker_host::{
    BrokerHostLimits, BrokerProviderRegistry, Streams, TestImports, asset::AssetInputs,
};
use dekopon_capability::{
    ExecutionConstraints, ProposedInvocation, StorageAccess, StorageConstraints, StorageInterface,
    StorageScope, broker::AuthorizationGate,
};
use dekopon_core::{Actor, AgentId, ExternalSubject, InvocationId, PrincipalId, TraceId};
use dekopon_storage_host::{ContinuityPolicy, StorageGrantRequest, StorageHost, StorageLimits};
use serde_json::{Value, json};

/// Fixed broker clock: 2026-10-14T20:00:00Z.
fn clock_at_epoch_millis(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

const T0: u64 = 1_792_008_000_000;

#[derive(Debug)]
struct Output {
    status: u8,
    stdout: Vec<u8>,
    stderr: String,
}

impl Output {
    fn lines(&self) -> Vec<Value> {
        let text = std::str::from_utf8(&self.stdout).expect("stdout is UTF-8");
        if text.is_empty() {
            return Vec::new();
        }
        assert!(text.ends_with('\n'), "output ends with a newline");
        text.lines()
            .map(|line| serde_json::from_str(line).expect("each line is one JSON object"))
            .collect()
    }
}

struct Harness {
    registry: BrokerProviderRegistry,
    storage: StorageHost,
    next_call: AtomicU64,
}

impl Harness {
    async fn open(root: &Path, component: &Path) -> Self {
        Self::open_with_limits(root, component, StorageLimits::default()).await
    }

    async fn open_with_limits(root: &Path, component: &Path, limits: StorageLimits) -> Self {
        let storage = StorageHost::open(root, limits).expect("storage opens");
        let registry = BrokerProviderRegistry::load_with_storage(
            [component.canonicalize().expect("component path")],
            BrokerHostLimits::default(),
            Some(storage.clone()),
        )
        .await
        .expect("component loads");
        Self {
            registry,
            storage,
            next_call: AtomicU64::new(0),
        }
    }

    async fn call(
        &self,
        capability: &str,
        input: Value,
        clock: SystemTime,
        access: StorageAccess,
    ) -> Output {
        self.try_call_stdin(capability, input, clock, access, None)
            .await
            .unwrap_or_else(|error| panic!("host refused invocation: {error:?}"))
    }

    async fn try_call(
        &self,
        capability: &str,
        input: Value,
        clock: SystemTime,
        access: StorageAccess,
    ) -> Result<Output, dekopon_broker_host::BrokerHostError> {
        self.try_call_stdin(capability, input, clock, access, None)
            .await
    }

    async fn try_call_stdin(
        &self,
        capability: &str,
        input: Value,
        clock: SystemTime,
        access: StorageAccess,
        stdin: Option<Vec<u8>>,
    ) -> Result<Output, dekopon_broker_host::BrokerHostError> {
        let call = self.next_call.fetch_add(1, Ordering::Relaxed);
        let invocation: InvocationId = format!("pledge-e2e-{call}").parse().unwrap();
        let agent: AgentId = "pledge-e2e-agent".parse().unwrap();
        let proposal = ProposedInvocation::new(
            invocation.clone(),
            capability.parse().unwrap(),
            Actor::Agent {
                agent: agent.clone(),
            },
            TraceId::new(*b"dekopon-testkit!").expect("trace id"),
            input,
        );
        let authorized = AuthorizationGate::new()
            .authorize(
                proposal,
                "pledge".parse().unwrap(),
                "pledge-e2e-decision".to_owned(),
                "pledge-e2e-broker".parse::<PrincipalId>().unwrap(),
                "pledge-e2e-policy".to_owned(),
                ExecutionConstraints {
                    asset: None,
                    timeout_ms: 10_000,
                    http: None,
                    storage: Some(StorageConstraints {
                        interface: StorageInterface::Jsonl,
                        access,
                        scope: StorageScope::Agent,
                        retention: Default::default(),
                        namespace: None,
                    }),
                    secret_use: None,
                },
            )
            .expect("authorization succeeds");
        let grant = self
            .storage
            .grant(StorageGrantRequest::new(
                invocation,
                capability.parse().unwrap(),
                "pledge".parse().unwrap(),
                StorageInterface::Jsonl,
                access,
                StorageScope::Agent,
                agent,
                ExternalSubject::slack("t0123abcd", "u0123abcd").unwrap(),
                "slack-socket-mode",
                "workspace-slack",
                "c0123abc",
                "c0123abc:1712345678.000100",
                ContinuityPolicy::AuthorityBound,
                Vec::new(),
            ))
            .expect("storage grant succeeds");
        let (host_end, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let capture = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut bytes).unwrap();
            bytes
        });
        let stdin_end = stdin.map(|bytes| {
            let (host, mut feeder) = std::os::unix::net::UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                use std::io::Write;
                drop(feeder.write_all(&bytes));
            });
            host.into()
        });
        let assets = AssetInputs {
            streams: Some(Streams {
                stdin: stdin_end,
                stdout: host_end.into(),
            }),
            ..Default::default()
        };
        let imports = TestImports {
            clock: Some(clock),
            ..Default::default()
        };
        let result = self
            .registry
            .invoke_with_test_imports(authorized, None, Some(grant), assets, Some(&imports))
            .await;
        let stdout = capture.join().unwrap();
        match result {
            Ok(output) => Ok(Output {
                status: 0,
                stdout,
                stderr: output.stderr,
            }),
            Err(failure) => match *failure.error {
                dekopon_broker_host::BrokerHostError::ProviderFailure {
                    status, stderr, ..
                } => Ok(Output {
                    status,
                    stdout,
                    stderr,
                }),
                error => Err(error),
            },
        }
    }

    async fn write(&self, capability: &str, input: Value, clock: SystemTime) -> Output {
        self.call(capability, input, clock, StorageAccess::ReadWrite)
            .await
    }

    async fn read(&self, capability: &str, input: Value, clock: SystemTime) -> Output {
        self.call(capability, input, clock, StorageAccess::ReadOnly)
            .await
    }

    async fn run_command(
        &self,
        argv: &[&str],
        piped: bool,
    ) -> dekopon_broker_host::CommandRunOutcome {
        let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        self.registry
            .run_command("pledge", &argv, piped)
            .await
            .expect("run-command succeeds")
    }
}

fn component() -> Option<PathBuf> {
    match std::env::var_os("DEKOPON_PROVIDER_COMPONENT") {
        None => {
            eprintln!(
                "skipping ledger e2e: DEKOPON_PROVIDER_COMPONENT is unset; run scripts/build-component.sh"
            );
            None
        }
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "DEKOPON_PROVIDER_COMPONENT does not name a file: {}",
                path.display()
            );
            Some(path)
        }
    }
}

fn temp_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "pledge-ledger-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    root.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn make_list_keep_stats_round_trip() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made = host
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "Confirm migration"}),
            now,
        )
        .await;
    assert_eq!((made.status, made.stderr.as_str()), (0, ""));
    let lines = made.lines();
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("p-") && id.len() == 8);
    assert_eq!(lines[0]["state"], "open");

    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines()[0]["id"], id);

    let kept = host
        .write("pledge.keep", json!({"id": id, "note": "done"}), now)
        .await;
    assert_eq!(kept.status, 0);
    assert_eq!(kept.lines()[0]["state"], "kept");

    let stats = host.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.status, 0);
    assert_eq!(
        stats.lines()[0],
        json!({"open":0,"kept":1,"dropped":0,"broken":0,"kept_late":0,"on_time_rate":1.0})
    );

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn make_list_drop_stats_round_trip() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let first = host
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "Confirm migration"}),
            now,
        )
        .await;
    assert_eq!(first.status, 0);
    let id_dropped = first.lines()[0]["id"].as_str().unwrap().to_owned();
    let second = host
        .write(
            "pledge.make",
            json!({"to": "bob", "due": "3d", "text": "Review docs"}),
            now,
        )
        .await;
    assert_eq!(second.status, 0);
    let id_open = second.lines()[0]["id"].as_str().unwrap().to_owned();

    let dropped = host
        .write(
            "pledge.drop",
            json!({"id": id_dropped, "reason": "no longer needed"}),
            now,
        )
        .await;
    assert_eq!(dropped.status, 0);
    assert_eq!(dropped.lines()[0]["state"], "dropped");
    assert_eq!(dropped.lines()[0]["reason"], "no longer needed");

    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    let lines = listed.lines();
    assert_eq!(lines.len(), 2);
    let by_id: std::collections::HashMap<&str, &Value> = lines
        .iter()
        .map(|line| (line["id"].as_str().unwrap(), line))
        .collect();
    assert_eq!(by_id[id_dropped.as_str()]["state"], "dropped");
    assert_eq!(by_id[id_dropped.as_str()]["reason"], "no longer needed");
    assert_eq!(by_id[id_open.as_str()]["state"], "open");

    let stats = host.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.status, 0);
    assert_eq!(
        stats.lines()[0],
        json!({"open":1,"kept":0,"dropped":1,"broken":0,"kept_late":0,"on_time_rate":null})
    );

    // Settling twice still fails `not-open` without writing.
    for (capability, input) in [
        ("pledge.keep", json!({"id": id_dropped})),
        ("pledge.drop", json!({"id": id_dropped, "reason": "again"})),
    ] {
        let again = host.write(capability, input, now).await;
        assert_eq!(again.status, 1);
        assert_eq!(again.stderr, "no open promise with that id\n");
        assert!(again.stdout.is_empty());
    }
    let stats = host.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.lines()[0]["dropped"], 1);
    assert_eq!(stats.lines()[0]["open"], 1);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn settle_twice_fails_not_open_and_writes_nothing() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made = host
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "t"}),
            now,
        )
        .await;
    let id = made.lines()[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        host.write("pledge.keep", json!({"id": id}), now)
            .await
            .status,
        0
    );

    for capability in ["pledge.keep", "pledge.drop"] {
        let input = if capability == "pledge.keep" {
            json!({"id": id})
        } else {
            json!({"id": id, "reason": "too late"})
        };
        let again = host.write(capability, input, now).await;
        assert_eq!(again.status, 1);
        assert_eq!(again.stderr, "no open promise with that id\n");
        assert!(again.stdout.is_empty());
    }
    let unknown = host
        .write("pledge.keep", json!({"id": "p-000000"}), now)
        .await;
    assert_eq!(unknown.status, 1);

    let stats = host.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.lines()[0]["kept"], 1);
    assert_eq!(stats.lines()[0]["open"], 0);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_settle_ids_fail_usage_and_write_nothing() {
    use dekopon_broker_host::CommandRunOutcome;
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made = host
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "t"}),
            now,
        )
        .await;
    assert_eq!(made.status, 0);

    for (capability, input) in [
        ("pledge.keep", json!({"id": "xyz"})),
        ("pledge.keep", json!({"id": "p-ZZZZZZ"})),
        ("pledge.keep", json!({"id": ""})),
        ("pledge.drop", json!({"id": "xyz", "reason": "r"})),
        ("pledge.drop", json!({"id": "p-ZZZZZZ", "reason": "r"})),
        ("pledge.drop", json!({"id": "", "reason": "r"})),
    ] {
        let refused = host.write(capability, input, now).await;
        assert_eq!(refused.status, 2);
        assert_eq!(
            refused.stderr,
            "the promise id is not p- followed by six hex characters\n"
        );
        assert!(refused.stdout.is_empty());
    }

    // Unknown-but-well-formed ids still yield `not-open`.
    let unknown = host
        .write("pledge.keep", json!({"id": "p-000000"}), now)
        .await;
    assert_eq!(unknown.status, 1);
    assert_eq!(unknown.stderr, "no open promise with that id\n");

    // The argv grammar refuses malformed ids at propose time, before any run.
    match host.run_command(&["keep", "xyz"], false).await {
        CommandRunOutcome::Failed { error } => assert_eq!(error.code, "usage"),
        outcome => panic!("component must refuse a malformed id, got {outcome:?}"),
    }

    // Nothing was written: the ledger still holds exactly the open promise.
    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines().len(), 1);
    assert_eq!(listed.lines()[0]["state"], "open");

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn due_window_overdue_and_fail_if_none() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    host.write(
        "pledge.make",
        json!({"to": "a", "due": "1h", "text": "soon"}),
        now,
    )
    .await;
    host.write(
        "pledge.make",
        json!({"to": "b", "due": "3d", "text": "later"}),
        now,
    )
    .await;

    let window = host
        .read(
            "pledge.due",
            json!({"within": "2h", "fail_if_none": true}),
            now,
        )
        .await;
    assert_eq!(window.status, 0);
    let lines = window.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["to"], "a");
    assert_eq!(lines[0]["overdue"], false);

    let idle = host
        .read(
            "pledge.due",
            json!({"within": "0m", "fail_if_none": true}),
            now,
        )
        .await;
    assert_eq!(idle.status, 1);
    assert_eq!(idle.stderr, "no promise is due\n");
    assert!(idle.stdout.is_empty());

    let later = clock_at_epoch_millis(T0 + 2 * 3_600_000);
    let overdue = host
        .read(
            "pledge.due",
            json!({"within": "0m", "fail_if_none": true}),
            later,
        )
        .await;
    assert_eq!(overdue.status, 0);
    let lines = overdue.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["overdue"], true);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_due_fails_usage_without_writing() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let bad = host
        .write(
            "pledge.make",
            json!({"to": "a", "due": "tomorrow", "text": "t"}),
            now,
        )
        .await;
    assert_eq!(bad.status, 2);
    assert!(bad.stderr.contains("RFC 3339"));
    assert!(bad.stdout.is_empty());

    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert!(listed.stdout.is_empty());

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_grant_denies_writes_but_serves_reads() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made = host
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "t"}),
            now,
        )
        .await;
    assert_eq!(made.status, 0);

    let listed = host
        .call("pledge.list", json!({}), now, StorageAccess::ReadOnly)
        .await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines().len(), 1);

    // The guest maps the denied append to `storage-failed`, and the host
    // independently rejects the whole invocation at the boundary.
    let denied = host
        .try_call(
            "pledge.make",
            json!({"to": "bob", "due": "2h", "text": "t"}),
            now,
            StorageAccess::ReadOnly,
        )
        .await;
    match denied {
        Err(dekopon_broker_host::BrokerHostError::StorageCallRejected {
            capability,
            reason,
            ..
        }) => {
            assert_eq!(capability.as_str(), "pledge.make");
            assert_eq!(reason, "denied");
        }
        other => panic!("read-only write must be host-rejected, got {other:?}"),
    }

    // Nothing was written: the ledger still holds exactly the first promise.
    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.lines().len(), 1);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn promises_survive_a_broker_restart() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let storage_root = root.join("storage");
    let now = clock_at_epoch_millis(T0);

    let first = Harness::open(&storage_root, &component).await;
    first
        .write(
            "pledge.make",
            json!({"to": "alice", "due": "2h", "text": "one"}),
            now,
        )
        .await;
    first
        .write(
            "pledge.make",
            json!({"to": "bob", "due": "3d", "text": "two"}),
            now,
        )
        .await;
    drop(first);

    let second = Harness::open(&storage_root, &component).await;
    let listed = second.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines().len(), 2);
    let stats = second.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.lines()[0]["open"], 2);

    let id = listed.lines()[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        second
            .write("pledge.keep", json!({"id": id}), now)
            .await
            .status,
        0
    );
    let stats = second.read("pledge.stats", json!({}), now).await;
    assert_eq!(stats.lines()[0]["kept"], 1);

    drop(second);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn make_reads_piped_stdin_and_validates_it() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let piped = host
        .try_call_stdin(
            "pledge.make",
            json!({"to": "alice", "due": "2h"}),
            now,
            StorageAccess::ReadWrite,
            Some(b"  piped promise\n".to_vec()),
        )
        .await
        .expect("invocation runs");
    assert_eq!(piped.status, 0);
    assert_eq!(piped.lines()[0]["text"], "piped promise");

    for (stdin, status, message) in [
        (Some(Vec::new()), 2, "promise text is required\n"),
        (None, 2, "promise text is required\n"),
        (
            Some(vec![b'x'; 281]),
            2,
            "promise text exceeds 280 UTF-8 bytes\n",
        ),
        (
            Some(vec![0xff, 0xfe]),
            1,
            "piped input could not be read as text\n",
        ),
    ] {
        let refused = host
            .try_call_stdin(
                "pledge.make",
                json!({"to": "alice", "due": "2h"}),
                now,
                StorageAccess::ReadWrite,
                stdin,
            )
            .await
            .expect("invocation runs");
        assert_eq!(refused.status, status);
        assert_eq!(refused.stderr, message);
        assert!(refused.stdout.is_empty());
    }

    // Only the first piped make wrote anything.
    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.lines().len(), 1);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_through_component_matches_native_proposals() {
    use dekopon_broker_host::CommandRunOutcome;
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;

    match host
        .run_command(&["make", "--to", "alice", "--due", "2h", "do it"], false)
        .await
    {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.make");
            assert_eq!(input, json!({"to": "alice", "due": "2h", "text": "do it"}));
        }
        outcome => panic!("component must propose make, got {outcome:?}"),
    }
    match host.run_command(&["due", "--within", "1h"], false).await {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.due");
            assert_eq!(input, json!({"within": "1h", "fail_if_none": false}));
        }
        outcome => panic!("component must propose due, got {outcome:?}"),
    }
    match host
        .run_command(&["keep", "p-7f3a9c", "--note", "done", "quickly"], false)
        .await
    {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.keep");
            assert_eq!(input, json!({"id": "p-7f3a9c", "note": "done quickly"}));
        }
        outcome => panic!("component must propose keep, got {outcome:?}"),
    }
    match host
        .run_command(
            &["drop", "p-7f3a9c", "--reason", "no", "longer", "needed"],
            false,
        )
        .await
    {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.drop");
            assert_eq!(
                input,
                json!({"id": "p-7f3a9c", "reason": "no longer needed"})
            );
        }
        outcome => panic!("component must propose drop, got {outcome:?}"),
    }
    match host.run_command(&["list", "--kept"], false).await {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.list");
            assert_eq!(
                input,
                json!({"state": "kept", "oldest_first": false, "overdue_only": false, "fail_if_none": false})
            );
        }
        outcome => panic!("component must propose list --kept, got {outcome:?}"),
    }
    match host
        .run_command(&["list", "--overdue", "--to", "alice"], false)
        .await
    {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.list");
            assert_eq!(
                input,
                json!({"to": "alice", "oldest_first": false, "overdue_only": true, "fail_if_none": false})
            );
        }
        outcome => panic!("component must propose list --overdue, got {outcome:?}"),
    }
    match host.run_command(&["stats"], false).await {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.stats");
            assert_eq!(input, json!({"by_recipient": false}));
        }
        outcome => panic!("component must propose stats, got {outcome:?}"),
    }
    match host.run_command(&["stats", "--by-recipient"], false).await {
        CommandRunOutcome::Proposed {
            capability, input, ..
        } => {
            assert_eq!(capability.as_str(), "pledge.stats");
            assert_eq!(input, json!({"by_recipient": true}));
        }
        outcome => panic!("component must propose stats --by-recipient, got {outcome:?}"),
    }
    match host
        .run_command(
            &["make", "--to", "alice", "--due", "someday", "do it"],
            false,
        )
        .await
    {
        CommandRunOutcome::Failed { error } => assert_eq!(error.code, "usage"),
        outcome => panic!("component must refuse a bad due, got {outcome:?}"),
    }
    match host.run_command(&["keep", "xyz"], false).await {
        CommandRunOutcome::Failed { error } => {
            assert_eq!(error.code, "usage");
            assert_eq!(
                error.message,
                "the promise id is not p- followed by six hex characters"
            );
        }
        outcome => panic!("component must refuse a malformed id, got {outcome:?}"),
    }
    match host.run_command(&["--help"], false).await {
        CommandRunOutcome::Rendered { status, stdout, .. } => {
            assert_eq!(status, 0);
            assert!(stdout.contains("pledge"));
        }
        outcome => panic!("component must render help, got {outcome:?}"),
    }
    match host.run_command(&["--version"], false).await {
        CommandRunOutcome::Rendered { status, stdout, .. } => {
            assert_eq!(status, 0);
            assert_eq!(stdout, format!("pledge {}\n", env!("CARGO_PKG_VERSION")));
        }
        outcome => panic!("component must render version, got {outcome:?}"),
    }

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn due_edges_empty_limit_recipient_and_past_due_make() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let idle = host
        .read("pledge.due", json!({"fail_if_none": true}), now)
        .await;
    assert_eq!(idle.status, 1);
    assert_eq!(idle.stderr, "no promise is due\n");
    let quiet = host.read("pledge.due", json!({}), now).await;
    assert_eq!(quiet.status, 0);
    assert!(quiet.stdout.is_empty());

    for (to, due) in [("a", "1h"), ("b", "2h"), ("a", "3h")] {
        host.write(
            "pledge.make",
            json!({"to": to, "due": due, "text": "t"}),
            now,
        )
        .await;
    }
    let filtered = host
        .read("pledge.due", json!({"within": "4h", "to": "a"}), now)
        .await;
    assert_eq!(filtered.status, 0);
    let lines = filtered.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["to"] == "a"));

    let bounded = host
        .read("pledge.due", json!({"within": "4h", "limit": 1}), now)
        .await;
    assert_eq!(bounded.status, 0);
    let lines = bounded.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["to"], "a");

    let oldest = host
        .read("pledge.list", json!({"oldest_first": true}), now)
        .await;
    let lines = oldest.lines();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines
            .iter()
            .map(|line| line["to"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "a"]
    );

    let missing = host
        .read(
            "pledge.list",
            json!({"to": "nobody", "fail_if_none": true}),
            now,
        )
        .await;
    assert_eq!(missing.status, 1);
    assert_eq!(missing.stderr, "no promise matches\n");

    // A past-due make is allowed and echoes back already overdue.
    let late = host
        .write(
            "pledge.make",
            json!({"to": "p", "due": "2020-01-01T00:00:00Z", "text": "t"}),
            now,
        )
        .await;
    assert_eq!(late.status, 0);
    assert_eq!(late.lines()[0]["overdue"], true);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_ledger_line_fails_reads_loudly() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let storage_root = root.join("storage");
    let host = Harness::open(&storage_root, &component).await;
    let now = clock_at_epoch_millis(T0);

    host.write(
        "pledge.make",
        json!({"to": "a", "due": "2h", "text": "t"}),
        now,
    )
    .await;

    let ledger = find_ledger_file(&storage_root);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&ledger)
        .unwrap()
        .write_all(b"not-json\n")
        .unwrap();

    for capability in ["pledge.list", "pledge.due", "pledge.stats"] {
        let broken = host.read(capability, json!({}), now).await;
        assert_eq!(broken.status, 1, "{capability}");
        assert_eq!(broken.stderr, "the promise ledger is corrupt\n");
        assert!(broken.stdout.is_empty());
    }

    // Writes fail just as loudly and append nothing.
    let before = std::fs::read(&ledger).unwrap();
    for (capability, input) in [
        ("pledge.make", json!({"to": "b", "due": "2h", "text": "t"})),
        ("pledge.keep", json!({"id": "p-000000"})),
        ("pledge.drop", json!({"id": "p-000000", "reason": "r"})),
    ] {
        let broken = host.write(capability, input, now).await;
        assert_eq!(broken.status, 1, "{capability}");
        assert_eq!(broken.stderr, "the promise ledger is corrupt\n");
        assert!(broken.stdout.is_empty(), "{capability}");
        assert_eq!(std::fs::read(&ledger).unwrap(), before, "{capability}");
    }

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_past_year_9999_fails_writes_in_wasm() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made = host
        .write(
            "pledge.make",
            json!({"to": "a", "due": "2h", "text": "t"}),
            now,
        )
        .await;
    assert_eq!(made.status, 0);

    // `MAX_MILLIS + 1`: the broker clock reads past year 9999.
    let far = clock_at_epoch_millis(253_402_300_800_000);
    let refused = host
        .write(
            "pledge.make",
            json!({"to": "b", "due": "2h", "text": "t"}),
            far,
        )
        .await;
    assert_eq!(refused.status, 1);
    assert_eq!(refused.stderr, "the broker clock is past year 9999\n");
    assert!(refused.stdout.is_empty());

    // The refused write appended nothing; normal operations still succeed.
    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines().len(), 1);
    let again = host
        .write(
            "pledge.make",
            json!({"to": "c", "due": "2h", "text": "u"}),
            now,
        )
        .await;
    assert_eq!(again.status, 0);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn oversize_ledger_fails_reads_in_wasm() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let storage_root = root.join("storage");
    // Raised host caps so the >16 MiB ledger reaches the guest's own fold
    // bound: with default host limits the invocation setup refuses first.
    let host = Harness::open_with_limits(
        &storage_root,
        &component,
        StorageLimits {
            max_file_bytes: 32 * 1024 * 1024,
            max_read_bytes_per_invocation: 32 * 1024 * 1024,
            ..StorageLimits::default()
        },
    )
    .await;
    let now = clock_at_epoch_millis(T0);

    host.write(
        "pledge.make",
        json!({"to": "a", "due": "2h", "text": "t"}),
        now,
    )
    .await;
    let listed = host.read("pledge.list", json!({}), now).await;
    assert_eq!(listed.status, 0);
    assert_eq!(listed.lines().len(), 1);

    let ledger = find_ledger_file(&storage_root);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        while std::fs::metadata(&ledger).unwrap().len() <= 16 * 1024 * 1024 {
            file.write_all(&chunk).unwrap();
        }
    }

    let broken = host.read("pledge.list", json!({}), now).await;
    assert_eq!(broken.status, 1);
    assert_eq!(
        broken.stderr,
        "the promise ledger exceeds the 16 MiB read bound\n"
    );
    assert!(broken.stdout.is_empty());

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

fn find_ledger_file(root: &Path) -> PathBuf {
    fn visit(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, found);
            } else if std::fs::read(&path)
                .unwrap()
                .windows(b"\"ev\":\"made\"".len())
                .any(|window| window == b"\"ev\":\"made\"")
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    visit(root, &mut found);
    assert_eq!(found.len(), 1);
    found.pop().unwrap()
}

fn valid_id(id: &str) -> bool {
    id.len() == 8
        && id.as_bytes()[0] == b'p'
        && id.as_bytes()[1] == b'-'
        && id.as_bytes()[2..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
}

#[tokio::test(flavor = "multi_thread")]
async fn minted_ids_are_unique_hex() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let mut ids = Vec::new();
    for _ in 0..20 {
        let made = host
            .write(
                "pledge.make",
                json!({"to": "a", "due": "2h", "text": "t"}),
                now,
            )
            .await;
        assert_eq!(made.status, 0);
        let id = made.lines()[0]["id"].as_str().unwrap().to_owned();
        assert!(valid_id(&id), "{id}");
        ids.push(id);
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 20);

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_by_recipient_reports_sorted_per_recipient_lines() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let made_bob = host
        .write(
            "pledge.make",
            json!({"to": "bob", "due": "1h", "text": "second"}),
            now,
        )
        .await;
    assert_eq!(made_bob.status, 0);
    let id_bob = made_bob.lines()[0]["id"].as_str().unwrap().to_owned();
    host.write(
        "pledge.make",
        json!({"to": "alice", "due": "1h", "text": "first"}),
        now,
    )
    .await;
    host.write(
        "pledge.keep",
        json!({"id": id_bob}),
        clock_at_epoch_millis(T0 + 30 * 60_000),
    )
    .await;

    let later = clock_at_epoch_millis(T0 + 2 * 3_600_000);
    let grouped = host
        .read("pledge.stats", json!({"by_recipient": true}), later)
        .await;
    assert_eq!(grouped.status, 0);
    assert_eq!(
        grouped.lines(),
        [
            json!({"to":"alice","open":1,"kept":0,"dropped":0,"broken":1,"kept_late":0,"on_time_rate":0.0}),
            json!({"to":"bob","open":0,"kept":1,"dropped":0,"broken":0,"kept_late":0,"on_time_rate":1.0}),
        ]
    );

    // `--to` without the flag still narrows to the single aggregate object.
    let narrowed = host
        .read("pledge.stats", json!({"to": "alice"}), later)
        .await;
    assert_eq!(narrowed.status, 0);
    assert_eq!(
        narrowed.lines(),
        [json!({"open":1,"kept":0,"dropped":0,"broken":1,"kept_late":0,"on_time_rate":0.0})]
    );

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn kept_late_surfaces_on_each_promise() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let early = host
        .write(
            "pledge.make",
            json!({"to": "a", "due": "1h", "text": "kept late"}),
            now,
        )
        .await;
    let id_early = early.lines()[0]["id"].as_str().unwrap().to_owned();
    let prompt = host
        .write(
            "pledge.make",
            json!({"to": "b", "due": "3h", "text": "kept on time"}),
            now,
        )
        .await;
    let id_prompt = prompt.lines()[0]["id"].as_str().unwrap().to_owned();
    host.write(
        "pledge.make",
        json!({"to": "c", "due": "3h", "text": "still open"}),
        now,
    )
    .await;

    let half_hour = clock_at_epoch_millis(T0 + 30 * 60_000);
    let kept = host
        .write("pledge.keep", json!({"id": id_prompt}), half_hour)
        .await;
    assert_eq!(kept.status, 0);
    assert_eq!(kept.lines()[0]["late"], false);

    let two_hours = clock_at_epoch_millis(T0 + 2 * 3_600_000);
    let late = host
        .write("pledge.keep", json!({"id": id_early}), two_hours)
        .await;
    assert_eq!(late.status, 0);
    assert_eq!(late.lines()[0]["late"], true);

    let listed = host.read("pledge.list", json!({}), two_hours).await;
    let lines = listed.lines();
    assert_eq!(lines.len(), 3);
    let late_by_id: std::collections::HashMap<&str, &Value> = lines
        .iter()
        .map(|line| (line["id"].as_str().unwrap(), &line["late"]))
        .collect();
    assert_eq!(late_by_id[id_early.as_str()], &json!(true));
    assert_eq!(late_by_id[id_prompt.as_str()], &json!(false));
    assert!(
        lines
            .iter()
            .any(|line| line["to"] == "c" && line["late"] == false)
    );

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn list_kept_dropped_and_overdue_filters() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;
    let now = clock_at_epoch_millis(T0);

    let kept = host
        .write(
            "pledge.make",
            json!({"to": "a", "due": "1h", "text": "kept"}),
            now,
        )
        .await;
    let id_kept = kept.lines()[0]["id"].as_str().unwrap().to_owned();
    let dropped = host
        .write(
            "pledge.make",
            json!({"to": "b", "due": "1h", "text": "dropped"}),
            now,
        )
        .await;
    let id_dropped = dropped.lines()[0]["id"].as_str().unwrap().to_owned();
    host.write(
        "pledge.make",
        json!({"to": "c", "due": "2020-01-01T00:00:00Z", "text": "overdue"}),
        now,
    )
    .await;

    assert_eq!(
        host.write("pledge.keep", json!({"id": id_kept}), now)
            .await
            .status,
        0
    );
    assert_eq!(
        host.write(
            "pledge.drop",
            json!({"id": id_dropped, "reason": "withdrawn"}),
            now
        )
        .await
        .status,
        0
    );

    let only_kept = host
        .read("pledge.list", json!({"state": "kept"}), now)
        .await;
    assert_eq!(only_kept.status, 0);
    assert_eq!(only_kept.lines().len(), 1);
    assert_eq!(only_kept.lines()[0]["id"], id_kept);

    let only_dropped = host
        .read("pledge.list", json!({"state": "dropped"}), now)
        .await;
    assert_eq!(only_dropped.status, 0);
    assert_eq!(only_dropped.lines().len(), 1);
    assert_eq!(only_dropped.lines()[0]["id"], id_dropped);

    let settled = host
        .read("pledge.list", json!({"state": "settled"}), now)
        .await;
    assert_eq!(settled.status, 0);
    assert_eq!(settled.lines().len(), 2);

    let overdue = host
        .read("pledge.list", json!({"overdue_only": true}), now)
        .await;
    assert_eq!(overdue.status, 0);
    let lines = overdue.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["to"], "c");

    // `--overdue` composes with `--to`, `--open`, `--limit`, `--fail-if-none`.
    let narrowed = host
        .read(
            "pledge.list",
            json!({"to": "c", "overdue_only": true, "fail_if_none": true}),
            now,
        )
        .await;
    assert_eq!(narrowed.status, 0);
    assert_eq!(narrowed.lines().len(), 1);
    let empty = host
        .read(
            "pledge.list",
            json!({"to": "a", "overdue_only": true, "fail_if_none": true}),
            now,
        )
        .await;
    assert_eq!(empty.status, 1);
    assert_eq!(empty.stderr, "no promise matches\n");

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_reports_half_when_one_kept_and_one_broken() {
    let Some(component) = component() else {
        return;
    };
    let root = temp_root();
    let host = Harness::open(&root.join("storage"), &component).await;

    let made_a = host
        .write(
            "pledge.make",
            json!({"to": "a", "due": "1h", "text": "first"}),
            clock_at_epoch_millis(T0),
        )
        .await;
    let id_a = made_a.lines()[0]["id"].as_str().unwrap().to_owned();
    host.write(
        "pledge.make",
        json!({"to": "b", "due": "1h", "text": "second"}),
        clock_at_epoch_millis(T0),
    )
    .await;
    host.write(
        "pledge.keep",
        json!({"id": id_a}),
        clock_at_epoch_millis(T0 + 30 * 60_000),
    )
    .await;

    let stats = host
        .read(
            "pledge.stats",
            json!({}),
            clock_at_epoch_millis(T0 + 2 * 3_600_000),
        )
        .await;
    assert_eq!(stats.status, 0);
    assert_eq!(
        stats.lines()[0],
        json!({"open":1,"kept":1,"dropped":0,"broken":1,"kept_late":0,"on_time_rate":0.5})
    );

    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
