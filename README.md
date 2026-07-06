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
EOF

./agentplane --profile /tmp/agentplane.env process-list
./agentplane --profile /tmp/agentplane.env file-list --path .
```

Profile files are plain `KEY=VALUE` files. They are not shell scripts and are not executed.

## Common Workflows

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

If a network request times out, retry `process-start` with the same `--process-id` and the
same arguments. AgentPlane will reconnect to the existing process instead of starting a
duplicate command.

For one-shot commands, `process-run` combines start/read/wait and returns the remote exit
code as the local exit code:

```bash
./agentplane --profile /tmp/agentplane.env process-run \
  --process-id check-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 1800 \
  --tail-on-error 65536 \
  -- \
  bash -lc 'cargo check'
```

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
  --chunk-size 4194304 \
  --atomic \
  --checksum sha256:<hex> \
  --resume
```

Wait for generated output:

```bash
./agentplane file-wait \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json \
  --min-bytes 1 \
  --stable-ms 1000 \
  --timeout-seconds 300
```

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
forms. The current built-in provider is NVIDIA GPU through `nvidia-smi`.

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
| Sync | `sync-run` |
| Processes | `process-start`, `process-run`, `process-get`, `process-list`, `process-read`, `process-write`, `process-terminate`, `process-cleanup` |
| Files | `file-read`, `file-stat`, `file-wait`, `file-write`, `file-upload`, `file-delete`, `file-find`, `file-list` |
| Accelerators | `accelerator-status`, `accelerator-preflight`, `accelerator-wait-idle`, `gpu-status`, `gpu-preflight`, `gpu-wait-idle` |
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
