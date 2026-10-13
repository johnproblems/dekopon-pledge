# Pledge example deployment: a promise-keeping agent on a local socket

One agent (`pledge-keeper`) that records the promises it makes with the pledge
provider, served over an owner-only local socket so you can talk to it from a
shell. Nothing here is a mock: both `check` commands below validate these
exact files, including the Cedar policy and the storage grants.

| File | What it is | Who reads it |
|---|---|---|
| [`dekopon.yaml`](dekopon.yaml) | The catalog: the agent and its standing orders | `dekopon-gatewayd` |
| [`broker.yaml`](broker.yaml) | Broker configuration: identities, storage, constraint sets | `dekopon-brokerd` |
| [`policies.cedar`](policies.cedar) | Who may do what, and through which gateway | `dekopon-brokerd` |
| [`gatewayd.yaml`](gatewayd.yaml) | Gateway configuration: local transport, model, route | `dekopon-gatewayd` |

## 1. Build the component

From the repository root:

```console
make build
```

This writes `pledge.wasm`, which `broker.yaml` points at relatively
(`../../pledge.wasm`), and refuses to finish if the component reaches 1 MiB.

## 2. Check both configurations

Checks bind nothing and read no credentials or storage roots. Run them before
booting either daemon, and again after any edit:

```console
cd examples/broker
dekopon-brokerd check broker.yaml
dekopon-gatewayd check gatewayd.yaml
```

Both print `ok` when the files are valid. The broker check loads the component
manifest, so it also proves the six capability names and the storage grants
line up with the build.

## 3. Point the runtime paths at yourself

The absolute `/home/operator/...` paths are runtime state. Replace `operator`
with your user (or any layout you prefer), keeping the hygiene the daemons
enforce:

```console
mkdir -p ~/.local/run/dekopon ~/.local/share/dekopon
# storage root the broker writes pledge namespaces under (must be 0700:
# anything group/world-visible is refused as an unsafe root at boot):
mkdir -p ~/.local/share/dekopon/pledge-storage
chmod 0700 ~/.local/run/dekopon ~/.local/share/dekopon/pledge-storage
chmod 0600 broker.yaml policies.cedar gatewayd.yaml
$EDITOR broker.yaml gatewayd.yaml   # socketPath, serverUid/uid, socketPath, storage rootPath, wakes path
```

Also serve a model at the `endpoint` in `gatewayd.yaml` (Ollama with `qwen3`
by default), or point it at any OpenAI-compatible endpoint.

## 4. Boot the broker, then the gateway

```console
dekopon-brokerd --config broker.yaml
dekopon-gatewayd --config gatewayd.yaml
```

The broker logs one `broker.decision` / `broker.execution` audit record per
call on stdout; nothing leaves the machine.

## 5. Talk to the agent

Send JSON lines over the owner-only gateway socket (one connection stays open
for the whole session; replies arrive as JSON lines):

```console
python3 - <<'EOF'
import json, socket
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("/home/operator/.local/run/dekopon/gatewayd-dev.sock".replace("operator", __import__("getpass").getuser()))
sock.sendall((json.dumps({"subject": "tel.15550001111", "text": "Remind me to check the deploy in an hour."}) + "\n").encode())
with sock.makefile() as lines:
    for line in lines:
        print(line, end="")
EOF
```

Try the ledger out:

- `Remind me to check the deploy in an hour.` — the agent runs
  `pledge make --to <you> --due 1h ...` and schedules a wake.
- `Anything you owe me?` — the agent runs `pledge list --open` and answers
  from the ledger.
- `The deploy is fine, you can drop that.` — the agent runs
  `pledge drop <id> --reason ...`.
- `How are you doing on follow-through?` — the agent runs `pledge stats`.

Restart the broker (`Ctrl-C`, boot again) and ask what it owes you: the
promises are still there. Storage scope is `private-conversation`, so each
sender's ledger is separate; reads run under a read-only grant and the host
rejects any write attempted under one.

## Wakes

The route sets `wakes: true`, offering the model the `wake` tool. Two forms:

- `schedule`: come back once after a delay with a note (used right after
  `pledge make` for a promise with a real deadline).
- `watch`: run a read-only probe script on an interval with no model; exit 0
  wakes the agent with the output, exit 1 keeps waiting. The standing orders
  give the pairing: `pledge due --within 0m --fail-if-none`, which exits 0
  while anything is due or overdue and 1 while the ledger is quiet.

Pledge itself runs no scheduler, daemon, timer, or polling loop: it only
answers when asked, and the `--fail-if-none` flag exists so a watch probe can
tell "something is due" from "all quiet".
