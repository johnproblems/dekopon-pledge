# AGENTS.md — working on Pledge

## What this is

A Dekopon WebAssembly provider (`dekopon-provider-sdk =0.46.1`, pinned
exactly). The guest reads three broker imports only: JSONL storage, the wall
clock, and entropy. It must stay that way: no HTTP, Assets, Spawn, or
Settings.

## Workflow

```console
make build   # pledge.wasm via scripts/build-component.sh (1 MiB gate)
make test    # fast suite, no component needed
make e2e     # full suite with DEKOPON_PROVIDER_COMPONENT set
make check   # the CI gate: fmt + clippy -D warnings + deny + machete + e2e
```

`cargo test` without the component env var runs the fast suites and skips the
component-bound ones with a notice; with it set to a missing file, they fail.

## Rules that have bitten before

- `propose` is pure: no clock, no entropy, no storage. Relative `--due`
  values are syntax-checked in `propose` and resolved in `run`. The host
  traps import reads outside an authorized `run`.
- The storage handle is `Storage<Jsonl>` (methods `size`, `read_chunk`,
  `append`, `replace`), not bare `Jsonl`. A moved size reports `Busy`, not
  `InvalidArgument`; `append` takes the record without a trailing newline and
  the host adds it.
- Every input struct needs `#[serde(deny_unknown_fields)]` or `manifest()`
  fails. `Code` names are kebab-case, nonzero exit, checked at compile time.
- Writes validate, then append exactly once. Reads never skip a bad line:
  corruption fails loudly as `ledger-corrupt`.
- Error messages never echo input. Output is one compact JSON object per line
  with a trailing newline.
- The testkit's `Harness` authorizes with `storage: None` and cannot drive
  storage-backed capabilities; storage e2e lives in `tests/ledger.rs` on
  `BrokerProviderRegistry::load_with_storage`. Testkit is used for
  `conformance` only.
- There is no `dekopon-run` CLI; the shipped binaries are
  `dekopon-brokerd` and `dekopon-gatewayd`. Validate example configs with
  their `check` subcommands.
- Pledge answers when asked. It runs no scheduler, daemon, timer, or polling
  loop; wake pairing is documented in the README, not implemented here.

## Verifying config edits

After touching `examples/broker/`, rebuild if needed and run from that
directory:

```console
dekopon-brokerd check broker.yaml && dekopon-gatewayd check gatewayd.yaml
```
