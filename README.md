# AgentPlane

AgentPlane is a small remote-development control plane for machines, containers, and
shared compute workspaces.

It lets an agent or developer keep the local checkout as the source of truth while running
builds, tests, profiling jobs, inference workloads, file operations, and hardware readiness
checks on a remote machine.

AgentPlane is useful when:

- code is edited locally, but execution must happen remotely
- the remote workspace is reachable through a direct URL, routed service URL, port forward, or managed network path
- long-running commands need reconnect-safe logs and explicit lifecycle control
- multiple agents share one remote environment or GPU pool
- a workflow needs machine-readable file, process, sync, and accelerator state

AgentPlane is not an SSH client, Kubernetes operator, or terminal emulator. It is a small
HTTP(S) server plus CLI for explicit remote workspace operations.

## Supported Targets

AgentPlane can run wherever the server binary can start and the client can reach its HTTP(S)
endpoint:

- bare-metal development machines
- virtual machines
- containers
- shared GPU or CPU workstations
- workspaces exposed through a routed service URL or port forward

Linux remote hosts are the primary target today. macOS is supported for local development
and client-side workflows.

## Features

- **Remote process sessions**
  Start, inspect, read, write to, terminate, and recover long-running non-PTY processes.
- **Local-to-remote sync**
  Push local worktree deltas, exact git ref snapshots, or committed deltas before running a command.
- **File operations**
  Read, write, find, list, stat, wait for files, and perform atomic writes with mode/checksum support.
- **Gateway-friendly client**
  Use a routed URL, repeatable custom headers, profiles, bounded payloads, and retry knobs.
- **Accelerator checks**
  Inspect GPU status, run preflight checks, and wait until selected devices are idle.
- **Shared agent mode**
  Coordinate multiple agents with task-scoped leases and lease headers.
- **Resource guardrails**
  Enforce allow-root boundaries, output retention limits, request limits, and safe cleanup defaults.

## How It Works

AgentPlane has two pieces:

- `agentplane server` runs on the remote machine, VM, container, or shared workspace.
- `agentplane` CLI runs locally and talks to the server over HTTP(S).

The server only operates under configured `--allow-root` directories. Most endpoints require
a server token. The `health` endpoint is intentionally available without a token so callers can
check reachability before sending credentials.

```text
local checkout + agentplane CLI
        |
        | HTTP(S), optional custom headers
        v
remote machine running agentplane server
        |
        v
allowed remote workspace roots
```

## Agent Quick Start

Use this flow when Codex, Claude Code, or another agent should edit locally and run on a
remote machine.

### 1. Deploy The Server

On the remote machine, pick an existing parent directory that may contain remote projects:

```bash
REMOTE_IP=$(hostname -I | awk '{print $1}')
TOKEN='replace-with-random-token'
ALLOW_ROOT='/workspace'
REMOTE_ROOT='/workspace/project'

mkdir -p "$ALLOW_ROOT"
echo "AgentPlane server: http://$REMOTE_IP:8765"
echo "Remote root: $REMOTE_ROOT"

./agentplane server \
  --listen 0.0.0.0 \
  --port 8765 \
  --allow-root "$ALLOW_ROOT" \
  --token "$TOKEN"
```

`REMOTE_ROOT` may be a new project directory. AgentPlane can create it during sync as long
as it is under `ALLOW_ROOT`.

### 2. Tell The Agent To Load The Skill

Use this prompt:

```text
Load the AgentPlane skill from this repository's SKILL/ directory.
Use AgentPlane for remote sync, command execution, file operations, process lifecycle,
GPU checks when available, and shared lease workflows when needed.
```

### 3. Give The Agent The Environment

Use this prompt:

```text
AgentPlane server: http://<REMOTE_IP>:8765
AgentPlane token: <TOKEN>
Remote root: /workspace/project
Local repo: /path/to/local/repo

Create a local profile at /tmp/agentplane.env with AP_SERVER, AP_TOKEN, and AP_REMOTE_ROOT.
Then sync the local repo to the remote root and run cargo test.
```

The profile is a local secret. Do not commit it.

The agent will typically initialize the remote project with:

```bash
agentplane --profile /tmp/agentplane.env sync-run \
  --repo /path/to/local/repo \
  --ref HEAD \
  --command 'cargo test'
```

## Quick Start

### Install

Build from source:

```bash
cargo build --release
```

For local development, you can also install the CLI from this checkout:

```bash
cargo install --path .
```

### Start A Remote Server

Copy the binary to the remote machine, then start the server there:

```bash
TOKEN='replace-with-random-token'
REMOTE_ROOT='/workspace/project'

./agentplane server \
  --listen 0.0.0.0 \
  --port 8765 \
  --allow-root "$REMOTE_ROOT" \
  --token "$TOKEN"
```

From the local machine, point the CLI at the service URL:

```bash
AP_SERVER='http://remote.example.com:8765'
AP_TOKEN='replace-with-random-token'
AP_REMOTE_ROOT='/workspace/project'

./agentplane health --server "$AP_SERVER"
./agentplane process-list --server "$AP_SERVER" --token "$AP_TOKEN"

# If the service is reachable only through SOCKS5 with remote DNS:
./agentplane process-list \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --socks5-hostname 127.0.0.1:1086
```

When a request fails to connect or times out while `--socks5-hostname` (or `AP_SOCKS5_HOSTNAME`)
is set, the error names the configured proxy address and reminds you to verify it is listening
and reachable, so a typo'd proxy port is obvious instead of looking like a server problem.

For HTTPS with a self-signed certificate, start the server with:

```bash
./agentplane server \
  --listen 0.0.0.0 \
  --port 8765 \
  --allow-root "$REMOTE_ROOT" \
  --token "$TOKEN" \
  --tls-mode self-signed \
  --tls-state-dir "$REMOTE_ROOT/.agentplane-tls"
```

Then pass the generated CA certificate to the local CLI, or use your own TLS termination in
front of AgentPlane.

### Use A Routed URL Or Custom Headers

If the service URL requires extra request headers, pass them explicitly:

```bash
AP_SERVER='https://gateway.example.com/workspaces/dev/agentplane'

./agentplane process-list \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --header 'X-Workspace-Context: example'
```

For repeated use, put connection settings in a profile:

```bash
cat > /tmp/agentplane.env <<'EOF'
AP_SERVER=https://gateway.example.com/workspaces/dev/agentplane
AP_TOKEN=replace-with-random-token
AP_REMOTE_ROOT=/workspace/project
AP_SOCKS5_HOSTNAME=127.0.0.1:1086
AP_HEADER_1=X-Workspace-Context: example
AP_CONNECT_RETRIES=5
AP_CONNECT_RETRY_DELAY_MS=1000
AP_LABEL=node13
AP_RUN_ID=run42
EOF

./agentplane --profile /tmp/agentplane.env process-list
./agentplane --profile /tmp/agentplane.env file-list --path .
```

Profile files are plain `KEY=VALUE` files. They are not shell scripts and are not executed.
Profiles can also carry a stable agent identity:

```text
AP_AGENT_ID=minimax-a
AP_AGENT_ID_FILE=/workspace/mnt/agents/minimax-a.id
```

CLI values take precedence over profile values: `--agent-id`, then `--agent-id-file`, then
`AP_AGENT_ID`, then `AP_AGENT_ID_FILE`. Sync lock conflict messages include this identity.

`AP_LABEL` is an optional human-readable node label (for example `node13` or `node14`) that
helps disambiguate output when you drive several profiles. `health` and `process-status`
merge the label and the server address into their JSON output, and `process-status --text`
prints a `# label=<label> server=<server>` header before the process lines. Override the
profile label per invocation with `--label`. The label is client-side only; it does not
change the request or the server response.

## Common Workflows

### Initialize A Remote Project Directory

```bash
./agentplane sync-init \
  --repo /path/to/local/repo \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT"
```

`sync-init` mirrors the current git worktree into the remote root for first-time setup. It
includes tracked files and unignored untracked files, skips ignored files and `.git`, and
removes remote files that are not part of the current project snapshot unless protected by
`--preserve-path`.

### Sync Local Changes And Run A Command

```bash
./agentplane sync-run \
  --repo /path/to/local/repo \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --command 'cargo test'
```

`sync-run` supports three source modes:

- default worktree delta: send local changes, untracked files, and tracked deletes
- `--ref <target>`: mirror one exact committed git ref
- `--ref <target> --base-ref <base>`: send only committed changes between two refs

File contents transferred by `sync-run` use the same chunked upload transport as
`file-upload`. The default sync chunk is 262144 bytes; use
`--upload-chunk-size <BYTES>` when a gateway requires a different per-request size.
`sync-init` and `sync-run` automatically acquire a TTL-backed sync session lock for the
remote root while transferring and applying files, so users do not pass session ids
manually. The CLI caches the session token in the system temp directory so the same
`--agent-id` can recover after a dropped client; other agent ids still fail fast until the
lock is released or expires.

Use `--preserve-path <path>` with `--ref` when remote cache directories should survive exact
mirror syncs:

```bash
./agentplane sync-run \
  --repo /path/to/local/repo \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --ref HEAD \
  --preserve-path target \
  --preserve-path .cache \
  --command 'cargo build --release'
```

### Run A Reconnect-Safe Process

```bash
./agentplane process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id build-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --output-bytes-limit 8388608 \
  --save-output-path logs/build-1.log \
  -- \
  bash -lc 'cargo build --release'
```

Read output incrementally:

```bash
./agentplane process-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id build-1 \
  --after-seq 0 \
  --wait-ms 1000 \
  --max-bytes 65536 \
  --text
```

Add `--follow` to keep reading until exit. The client automatically attempts the authenticated
`/v1/events` WebSocket (including configured gateway headers) and falls back to cursor-based HTTP
reads if the upgrade is unsupported or the stream disconnects. SOCKS, custom CA, and insecure-TLS
profiles automatically use HTTP.

Transport selection is not a command option. For client diagnostics only, set
`AP_PROCESS_TRANSPORT=auto|http|websocket` in the client process environment. The default is
`auto`; `websocket` requires the upgrade and surfaces an error instead of falling back.

If a network request times out, retry `process-start` with the same `--process-id` and the
same arguments. AgentPlane will reconnect to the existing process instead of starting a
duplicate command.

If `process-read` cannot find a `--process-id` (for example, after a restart loses the id), the
error output includes a `hint:` pointing at `process-status`, which lists the most recently
active processes so you can recover the id instead of guessing.

Use `process-start` for long-running producers, samplers, servers, and benchmarks that
should keep running while you do other work. Use `process-run` for short build/check
commands and consumers/drivers where the local exit code should match the remote command.

For long jobs, add `--save-output-path <relative-path>` to `process-start` or
`process-run` to keep a full stdout/stderr copy under the remote root even when the
in-memory output buffer is truncated.

For one-shot commands, `process-run` combines start/read/wait and returns the remote exit
code as the local exit code. It uses the same automatic WebSocket-to-HTTP fallback:

```bash
./agentplane --profile /tmp/agentplane.env process-run \
  --process-id check-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 1800 \
  --tail-on-error 65536 \
  -- \
  bash -lc 'cargo check'
```

When the remote command exits non-zero, `--tail-on-error <BYTES>` prints the last retained
output bytes to stderr. Add `--tail-on-error-head-bytes <BYTES>` (default 512) to also print
the earliest retained bytes first, so the banner/env context that a tail-only view loses stays
visible; set `0` to disable the head.

### Check Long-Running Task Status

Use `process-status` to check whether a background task is still running, has exited, or
failed — without reading the full output:

```bash
# Check a single process by id
./agentplane process-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id build-1

# List the N most recently active processes (default: 10)
./agentplane process-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --limit 5
```

The response includes `status` (`running`, `exited`, or `failed`), `pid`, `exit_code`,
`elapsed_ms`, `started_at_unix_ms`, `last_output_at_unix_ms`, and the command summary.
When listing without `--process-id`, processes are sorted by most recent activity
(`last_output_at_unix_ms` falling back to `started_at_unix_ms`, descending).

`process-start` responses include `next_commands` with ready-to-use `process-status`,
`process-read`, and `process-terminate` command templates referencing the process id.
The token is replaced with a `<token>` placeholder so the output is safe to log.

### Group Processes Into A Run

Multi-node experiments (a producer on node14, a consumer + sampler on node13) share a
`run_id` so you can list and manifest them together. Set `--run-id` (or `AP_RUN_ID` in a
profile) on `process-start` / `process-run`; the value is echoed by `process-status` /
`process-get` / `process-list` and is the join key for `run-show` / `run-manifest`.

```bash
# One process per node, same run_id, full logs saved under runs/run42/
./agentplane process-start --profile /tmp/node14.env \
  --process-id run42-producer --run-id run42 \
  --save-output-path runs/run42/producer.log -- python3 train.py
./agentplane process-start --profile /tmp/node13.env \
  --process-id run42-consumer --run-id run42 \
  --save-output-path runs/run42/consumer.log -- python3 eval.py

# List only processes in this run on one node
./agentplane process-status --profile /tmp/node14.env --run-id run42

# Aggregate across nodes and write a local manifest cache
./agentplane run-show run42 \
  --profile /tmp/node14.env --profile /tmp/node13.env

# Export the manifest (reads the cache; no --profile needed once cached)
./agentplane run-manifest run42 --out runs/run42/manifest.json
```

`run-show` queries each profile's `process-list` (filtered server-side by `run_id`), joins
each process with its `save_output_path`, and prints a per-node view tagged with the
`AP_LABEL`. With no `--profile`, it reuses the profiles recorded in the cached manifest;
`--rebuild` reconstructs the manifest from server state alone. The manifest cache lives at
`$AP_RUN_DIR/<run_id>.json` (default `~/.agentplane/runs/`); it is a cache, not the source
of truth — the servers are. A retry of `process-start` with the same `--process-id` but a
different `--run-id` is rejected (reconnect-safe), matching the existing `--save-output-path`
rule.

### Manage Process Trees

Start a wrapper in its own process group:

```bash
./agentplane process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id service-1 \
  --kill-tree-on-terminate \
  -- \
  bash -lc './run-service.sh'
```

Terminate the group:

```bash
./agentplane process-terminate \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id service-1 \
  --tree
```

For broad cleanup, preview first. `process-cleanup` is dry-run by default and only sends a
signal when `--kill --signal TERM|KILL` is explicit.

```bash
./agentplane process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'service-name|worker-name' \
  --dry-run \
  --text
```

After a `--kill`, the server automatically polls the matcher again and sets the JSON `verified`
field when every signaled PID has exited. The settle window is bounded by
`--reconfirm-wait-ms` (default 2000 ms, with a server-side maximum); text mode prints a
`Reconfirm:` verdict line. Use `--no-reconfirm` only when the caller cannot wait for verification.

```bash
./agentplane process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'service-name|worker-name' \
  --kill \
  --signal TERM
```

Add `--accelerator-summary gpu|npu` to a `--dry-run` to attach per-PID accelerator occupancy
(device index/name plus used/total device memory) for the matched processes, so a residual
`xgl|vllm|mooncake|msprof|nsys` report shows what each process holds before you decide to kill.
The summary is server-side (it runs `nvidia-smi` / `npu-smi`), degrades to `available: false`
with a `reason` and warning when the provider is missing or fails, and only lists PIDs that are
both matched and reported by the provider as holding device memory. Ignored for `--kill`.

```bash
./agentplane process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|mooncake' \
  --dry-run \
  --accelerator-summary npu \
  --text
```

### File Operations

Write a text file:

```bash
./agentplane file-write \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path config/dev.txt \
  --content 'hello'
```

Upload local bytes atomically with a mode and checksum:

```bash
./agentplane file-write \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path bin/tool \
  --from-local ./target/tool \
  --atomic \
  --mode 755 \
  --checksum sha256:<hex>
```

Upload a large local file in chunks with resume support:

```bash
./agentplane file-upload \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path models/weights.bin \
  --from-local ./models/weights.bin \
  --chunk-size 1048576 \
  --atomic \
  --checksum sha256:<hex> \
  --resume
```

The default `--chunk-size` is 1048576 bytes (1 MiB). The client automatically sends each chunk
as `application/octet-stream`, then falls back to the original JSON/base64 route when an older
server or gateway rejects the raw endpoint. A raw `413 Payload Too Large` is not retried as
base64 because that request would be larger; choose a smaller chunk instead. For client
diagnostics only, set `AP_UPLOAD_TRANSPORT=auto|json|binary` in the client process environment.

Use `--lock-key <KEY>` when multiple agents may upload the same logical artifact and should
fail fast instead of racing on the same target. If the client dies mid-upload, rerun with
the same `--agent-id` and `--lock-key` to recover the existing upload session; a different
agent id remains blocked.

Copy a single file between two profiles (for example node13 and node14) in one step, instead
of `file-read` to a local temp file, `file-write` to the other node, and cleaning up by hand.
Each side is described by its own `--profile` file; no tokens are passed on the command line,
and the destination uploads in chunks through the same transport as `file-upload`:

```bash
./agentplane file-copy \
  --from-profile /tmp/node14.env \
  --from-path metadata.json \
  --to-profile /tmp/node13.env \
  --to-path metadata.json \
  --checksum
```

`--from-remote-root` / `--to-remote-root` override each profile's `AP_REMOTE_ROOT`,
`--chunk-size` sizes the chunked upload (default 1048576), `--atomic` writes the destination
atomically, and `--checksum` stats the destination after the copy and verifies its SHA-256
matches the source. Only single files are supported in this version; the source is pulled with
`file-read`, so very large sources are bounded by that single-response transport. Destination
uploads select their transport automatically; downloads remain JSON/base64 in this version.

Wait for generated output:

```bash
./agentplane file-wait \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json \
  --min-bytes 1 \
  --stable-ms 1000 \
  --timeout-seconds 300 \
  --process-id build-1
```

On timeout, `file-wait` prints the last observed path state (exists/size/modified time) to
stderr. With `--process-id <ID>` it also probes that producer process and reports its status,
exit code, and whether it is still alive, so you can tell a dead producer from a slow one.

### GPU Readiness

Inspect GPU state:

```bash
./agentplane gpu-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --text
```

Block launch when selected devices are busy:

```bash
./agentplane gpu-preflight \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --forbid-match 'service-name|worker-name'
```

Wait until devices are stably idle:

```bash
./agentplane gpu-wait-idle \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --stable-seconds 10 \
  --timeout-seconds 180
```

`accelerator-status`, `accelerator-preflight`, and `accelerator-wait-idle` are the generic
forms. Built-in providers are NVIDIA GPU through `nvidia-smi` and Huawei Ascend NPU through
`npu-smi`; use `npu-status` as the NPU status shortcut.

### Shared Agent Mode

Shared mode coordinates multiple agents that operate on one remote machine or shared
workspace.

Acquire a lease:

```bash
./agentplane mode-switch \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --mode shared \
  --task-id task-1 \
  --lease-id lease-1 \
  --ttl-seconds 300
```

Pass the lease headers on execution requests:

```bash
./agentplane process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id task-1-build \
  --header 'x-agentplane-agent-mode: shared' \
  --header 'x-agentplane-task-id: task-1' \
  --header 'x-agentplane-lease-id: lease-1' \
  -- \
  bash -lc 'make build'
```

When a shared-mode command should reserve a specific resource, add repeatable `--claim`
flags such as `--claim gpu:0`, `--claim gpu:0,1`, or `--claim port:6006`. Claims are
checked only in shared mode with an active lease. `CUDA_VISIBLE_DEVICES` remains a
backward-compatible GPU inference path, but explicit claims cover workloads that choose
resources internally instead of through environment variables.

Renew or release the lease at task boundaries:

```bash
./agentplane lease-renew \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id task-1 \
  --lease-id lease-1

./agentplane lease-release \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id task-1 \
  --lease-id lease-1
```

## Command Overview

| Area | Commands |
| --- | --- |
| Connectivity | `health` |
| Sync | `sync-init`, `sync-run` |
| Processes | `process-start`, `process-run`, `process-get`, `process-list`, `process-read`, `process-write`, `process-terminate`, `process-cleanup` |
| Files | `file-read`, `file-stat`, `file-wait`, `file-write`, `file-upload`, `file-copy`, `file-delete`, `file-find`, `file-list` |
| Accelerators | `accelerator-status`, `accelerator-preflight`, `accelerator-wait-idle`, `gpu-status`, `gpu-preflight`, `gpu-wait-idle` |
| Runs | `run-show`, `run-manifest` |
| Shared mode | `mode-get`, `mode-switch`, `lease-renew`, `lease-release` |
| Server | `server` |

Run `./agentplane <command> --help` for command-specific flags.

## Security Model

AgentPlane is meant to be small and explicit rather than ambiently powerful.

- Business endpoints require the server token.
- `health` does not require a token.
- Every file and process request is constrained by server-side `--allow-root`.
- Paths are validated to prevent escaping the allowed roots.
- File writes support atomic replacement and checksum verification.
- Process cleanup is preview-first and requires explicit signal flags before killing.
- Custom headers and tokens should be passed through local profiles or environment-specific
  secret handling, not committed to source control.

## Resource Defaults

The server defaults are conservative for shared remote machines and workspaces:

- max running processes: `8`
- max retained exited processes: `32`
- default per-process output retention: `1 MiB`
- hard max per-process output retention: `8 MiB`
- default `process-read` payload cap: `64 KiB`
- hard max `process-read` payload cap: `1 MiB`
- hard max raw upload chunk body: `8 MiB`
- max single `process-write` stdin payload: `64 KiB`
- max process timeout: `24h`
- exited process retention TTL: `600s`

Tune these with `agentplane server --help` when the deployment needs larger limits.

## Development

Format, lint, and test:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Project layout:

```text
src/cli/       CLI arguments and client command implementations
src/server/    HTTP server, request handlers, process/file/accelerator logic
src/protocol/  Request and response DTOs
tests/         End-to-end and integration tests
SKILL/         Optional agent-facing operating instructions
```

## Current Limits

- process sessions are non-PTY
- there is no terminal resize or terminal emulation
- retry policy is conservative and only applies to safe client requests
- NPU support includes a built-in Huawei Ascend provider through `npu-smi`
- GPU support currently depends on `nvidia-smi`

## Agent Skill

The `SKILL/` directory contains optional instructions for agent runtimes that know how to
load local skills. It is not required for using the CLI, but it helps agents choose safe
defaults for sync, process lifecycle, file operations, gateway headers, and GPU readiness
checks.
