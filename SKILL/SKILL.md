---
name: agentplane
description: Operate a remote machine, container, or shared workspace for build, test, inference, file editing, long-lived process sessions, and optional multi-agent GPU lease workflows via `agentplane`. Use this when the workflow is "edit locally, run remotely" against either a direct AgentPlane server or a path-based gateway URL that also requires custom gateway headers.
---

# AgentPlane

Use this skill when the user wants Codex to operate a remote machine, container, or shared
workspace while keeping a local checkout as the source of truth.

This skill supports two **access modes**. Pick exactly one before any remote operation:

1. **Direct mode**
   The client can talk to `agentplane` directly with `--server` and `--token`.
2. **Gateway mode**
   The server is reachable only through a routed service URL and the gateway also requires
   additional request headers.

Access mode is separate from execution mode:

- access mode: `direct` or `gateway`
- execution mode: default single-agent, or optional shared mode with GPU leases

Prefer terminal commands and direct HTTP. Do not use browser automation unless the user
explicitly asks for it.

## What you need

Always identify these values first:

- local CLI path or command
- remote service base URL
- service token
- remote project root inside the container

**If any value is missing or cannot be derived from what the user already gave you, stop and
ask the user for it — do not guess.** This applies above all to secrets and endpoint
details: never try to discover `AP_TOKEN` by attempting common values, and do not port-scan
to find the service. Confirming a user-supplied URL with a single `health` probe is
verification; fuzzing unknown tokens, ports, or paths is guessing and is not allowed.

Useful shell variables:

```bash
export AP_BIN="${AP_BIN:-agentplane}"
export AP_ACCESS_MODE='<direct|gateway>'
export AP_SERVER='<direct server url or routed service root>'
export AP_TOKEN='<server token>'
export AP_REMOTE_ROOT='/abs/path/inside/container'
export AP_TASK_ID="${AP_TASK_ID:-}"
export AP_LEASE_ID="${AP_LEASE_ID:-}"
```

When the installed CLI supports profiles, prefer a local env/profile file for repeated
gateway-backed work instead of copying long command lines:

```bash
cat > /tmp/agentplane-gateway.env <<'EOF'
AP_SERVER=https://gateway.example.com/workspaces/dev/agentplane
AP_TOKEN=replace-me
AP_REMOTE_ROOT=/workspace/mnt
AP_HEADER_1=X-Workspace-Context: example
AP_HEADER_2=X-Request-Context: example
AP_HEADER_3=X-Client-Context: example
AP_CONNECT_RETRIES=5
AP_CONNECT_RETRY_DELAY_MS=3000
EOF
```

Use it as:

```bash
"$AP_BIN" --profile /tmp/agentplane-gateway.env process-list
"$AP_BIN" --env-file /tmp/agentplane-gateway.env file-list --path .
```

The profile parser accepts simple `KEY=VALUE` lines only and does not execute shell code.
Command-line values still override profile values. Treat profile files as secrets and do not
write them into project source directories.

Local direct-server E2E verification has covered this profile flow with `health`,
`process-list`, `file-write`, `file-list`, `file-read`, and `process-run`, including remote
exit-code propagation. Gateway header mapping from profile variables is covered by the CLI
integration tests; still run a fresh Gateway smoke test when required custom headers
change.

For Gateway mode, also capture:

```bash
export AP_HEADER_1='<Header-Name: value>'
export AP_HEADER_2='<Another-Header: value>'
```

`AP_SERVER` in Gateway mode should be the service root, for example:

```text
https://gateway.example.com/workspaces/dev/agentplane
```

not the workspace page URL.

## Choose the mode

Mode selection is mandatory. Before running any command that touches the remote container,
the agent must know and state the current access mode:

```text
Access mode: direct | gateway
Evidence: <direct service URL works> | <gateway URL plus required custom headers>
Execution mode: single-agent | shared lease
```

If the evidence is ambiguous, do one safe `health` probe against the user-supplied
`AP_SERVER`. Do not scan ports, guess gateway routes, or try token values.

Use **Direct mode** when normal CLI commands can reach the service root directly and no
custom gateway headers is required.

Use **Gateway mode** when the user gives:

- a routed service URL
- a representative browser `curl` request
- required custom request headers
- or any evidence that a gateway redirects unauthenticated requests to login

After choosing, set:

```bash
export AP_ACCESS_MODE='direct'
# or:
export AP_ACCESS_MODE='gateway'
```

Do not mix access modes in one command. Gateway commands must carry required custom
headers; Direct mode commands normally must not depend on gateway-only headers.

## Direct mode

### Reachability and proxy hygiene

Corporate environments often inject `http_proxy` or `https_proxy`. Bypass the host in
`AP_SERVER` before assuming the service is down:

```bash
AP_HOST=$(printf '%s' "$AP_SERVER" | sed -E 's#^[a-zA-Z][a-zA-Z0-9+.-]*://##; s#[:/].*$##')
export NO_PROXY="${NO_PROXY:+$NO_PROXY,}$AP_HOST"
export no_proxy="$NO_PROXY"
```

Probe first:

```bash
"$AP_BIN" health --server "$AP_SERVER"
```

Notes:

- `health` does not need `--token`
- timeout or HTML proxy pages usually mean local proxy interception

### Built-in retry knobs

Recent `agentplane` builds support two client-side retry flags on safe commands:

- `--connect-retries <N>`
- `--connect-retry-delay-ms <MS>`

They apply to safe requests such as:

- `health`
- `accelerator-status`
- `gpu-status`
- `accelerator-preflight`
- `gpu-preflight`
- `accelerator-wait-idle`
- `gpu-wait-idle`
- `process-start`
- `process-get`
- `process-list`
- `process-read`
- `process-terminate`
- `file-read`
- `file-stat`
- `file-wait`
- `file-find`
- `file-list`

They do **not** apply to mutating writes such as:

- `process-write`
- `file-write`
- `file-delete`
- `sync-run`

Recommended starting points:

- normal direct mode: `--connect-retries 3 --connect-retry-delay-ms 1000`
- flaky but short outages: `--connect-retries 5 --connect-retry-delay-ms 3000`

For long-running jobs, prefer retrying `process-start` with the same stable `--process-id`
instead of inventing a new ID. That preserves idempotency and avoids double execution.

### Normal operations

Use the CLI directly for:

- `sync-run`
- `accelerator-status`
- `gpu-status`
- `accelerator-preflight`
- `gpu-preflight`
- `accelerator-wait-idle`
- `gpu-wait-idle`
- `process-start`
- `process-run`
- `process-read`
- `process-get`
- `process-list`
- `process-write`
- `process-terminate`
- `process-cleanup`
- `file-read`
- `file-stat`
- `file-wait`
- `file-write`
- `file-find`
- `file-list`
- `file-delete`

Common pattern:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id build-1 \
  --connect-retries 3 \
  --connect-retry-delay-ms 1000 \
  --output-bytes-limit 8388608 \
  -- bash -lc 'pwd; uname -a; nvidia-smi -L || true'
```

For wrappers that start background services, use process-tree cleanup:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id nsys-xgl-c1 \
  --kill-tree-on-terminate \
  --output-bytes-limit 8388608 \
  -- bash -lc './target/release/xgl ... & evalscope perf ...; wait'

"$AP_BIN" process-terminate \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id nsys-xgl-c1 \
  --tree
```

On Unix, `--kill-tree-on-terminate` starts the wrapper in its own process group and
`process-terminate --tree` sends termination to that group. Use this for XGL/vLLM/nsys or
EvalScope wrappers that might otherwise leave GPU processes behind. `process-get` and
`process-list` include the tree cleanup flag, process group id, and `children_running` when
the wrapper has exited but same-group children are still alive. If the server was started
with `--default-kill-tree-on-terminate`, every new `process-start` uses this cleanup mode by
default.

Local direct-server E2E verification has covered both process-tree cleanup paths:

- explicit `process-start --kill-tree-on-terminate` followed by `process-get` observing
  `children_running=true` after the wrapper exits, then `process-terminate --tree` cleaning
  the same process group
- server `--default-kill-tree-on-terminate` causing a plain `process-start` to create a
  process group and allowing plain `process-terminate` to clean a TERM-ignoring background
  child through the SIGKILL fallback

For orphaned or externally started processes, use `process-cleanup` with a narrow command
match. Always dry-run first and review the matched PIDs before sending a signal:

```bash
"$AP_BIN" process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|evalscope' \
  --dry-run \
  --text

"$AP_BIN" process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|evalscope' \
  --kill \
  --signal TERM
```

`process-cleanup` is dry-run by default unless `--kill` is present. Actual signaling
requires `--kill --signal TERM` or `--kill --signal KILL`; `--signal` by itself is
rejected. Matching is case-insensitive substring matching against process commands, with
`|` separating alternatives. Do not use broad matches such as `python`, `bash`, or `server`
unless the user explicitly confirms the matched dry-run report. The server skips the
AgentPlane server process and cleanup client commands if they match the search term.
Local direct-server CLI E2E verification has covered dry-run no-signal behavior, missing
signal rejection, explicit `TERM` cleanup, and self-protection.

For one remote command where you want decoded logs streamed until exit and the local CLI exit
code to match the remote process, prefer `process-run`:

```bash
"$AP_BIN" --profile /tmp/agentplane-gateway.env process-run \
  --process-id build-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 1800 \
  --output-bytes-limit 8388608 \
  --tail-on-error 65536 \
  -- bash -lc 'make build'
```

`process-run` is a client-side composition of `process-start` plus repeated
`process-read`. Use a stable `--process-id`; repeating the same command with the same
configuration can reconnect to the existing process instead of starting duplicate work. Use
`--tail-on-error <BYTES>` when failures should include the last retained output on stderr.
If the CLI reports that the output cursor expired, earlier logs were already trimmed; rerun
with a larger `--output-bytes-limit` or higher server output retention limits.

`process-read` and `file-read` return base64 by default. Prefer the CLI text helpers when
available:

```bash
"$AP_BIN" file-read --server "$AP_SERVER" --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" --path README.md --text

"$AP_BIN" process-read --server "$AP_SERVER" --token "$AP_TOKEN" \
  --process-id build-1 --text --tail 4000
```

If the installed binary is older and lacks `--text`, decode manually.

For generated artifacts, prefer `file-stat` and `file-wait` instead of hand-written
`ls`/`find`/`sleep` loops:

```bash
"$AP_BIN" file-stat \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json

"$AP_BIN" file-wait \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json \
  --min-bytes 1 \
  --stable-ms 1000 \
  --timeout-seconds 300
```

`file-stat` returns JSON for both existing and missing paths, including `exists`,
`file_type`, `size`, `modified_unix_ms`, `executable`, and regular-file `sha256`.
`file-wait` polls `file-stat` until the path exists, optional `--min-bytes` is satisfied,
and optional `--stable-ms` observes an unchanged size. On timeout it exits non-zero and
prints the last observed state to stderr. Local direct-server CLI E2E verification has
covered missing `file-stat`, `file-wait --min-bytes`, `file-wait --stable-ms`, timeout
reporting, and SHA-256 reporting.

For targeted protected writes, prefer `file-write --from-local` over large inline
`--content` payloads:

```bash
"$AP_BIN" file-write \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path bin/tool \
  --from-local ./target/tool \
  --atomic \
  --mode 755 \
  --checksum sha256:<hex>
```

`--from-local` uploads arbitrary local bytes. `--atomic` writes a same-directory temp file
and renames it into place. `--mode <OCTAL>` sets exact Unix permissions.
With `--from-local`, `--preserve-mode` applies the local file mode when no explicit mode is
supplied; with inline `--content`, it preserves the existing remote target mode. Parent
directory creation remains enabled by default and can be made explicit with
`--create-parents`.

### Accelerator status

`agentplane` treats accelerator inspection as a generic module. Use
`accelerator-status --kind gpu` for the generic entrypoint, or `gpu-status` as the GPU
shortcut. The first implemented provider is NVIDIA through `nvidia-smi`; NPU providers are
future extensions.

Probe at most once per container session to establish whether GPU hardware exists unless
the user says the environment changed:

```bash
"$AP_BIN" accelerator-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --kind gpu

"$AP_BIN" gpu-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --text
```

The default output is JSON. `--text` is a compact human summary. When no NVIDIA GPU is
available, the command still exits successfully and returns `available:false`,
`provider:null`, a `reason`, and an `agent_hint`. Treat that as a stable fact for the
current remote container session; do not repeatedly call GPU status, preflight, wait, or
cleanup commands after `available:false` unless the user explicitly says the hardware or
container changed.

When `available:true`, use the returned devices and processes to decide whether GPU work is
safe to start. Device records include memory, utilization, pstate, power, temperature, and
UUID when reported by `nvidia-smi`. Process records include GPU index, PID, used memory, and
best-effort `ps` details such as PPID, PGID, SID, elapsed time, user, stat, and command.

Before starting XGL, vLLM, nsys, EvalScope, or similar GPU workloads, prefer `gpu-preflight`
over hand-parsing `gpu-status` JSON:

```bash
"$AP_BIN" gpu-preflight \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --forbid-match 'xgl|vllm|nsys|evalscope'
```

`gpu-preflight` exits non-zero when a selected GPU is missing, memory or utilization is
above threshold, metrics are unavailable, or a compute process command matches the
case-insensitive `--forbid-match` regex. Failure output identifies the GPU, PID, command,
and threshold that blocked startup. Use `accelerator-preflight --kind gpu` when you want the
generic accelerator entrypoint.

After stopping wrappers, profilers, or model servers, prefer `gpu-wait-idle` before the next
run:

```bash
"$AP_BIN" gpu-wait-idle \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --stable-seconds 10 \
  --timeout-seconds 180 \
  --forbid-match 'xgl|vllm|nsys|evalscope'
```

`gpu-wait-idle` loops over GPU status until all selected GPUs stay within thresholds for
`--stable-seconds`. It exits non-zero on timeout and includes the last GPU/process snapshot
so the blocking PID and command are visible. Use `accelerator-wait-idle --kind gpu` for the
generic accelerator form. If `gpu-status` has already returned `available:false` in the
same container session, do not call these GPU readiness helpers unless the user says the
environment changed.

### Shared mode and GPU leases

Default mode is **single-agent**. Do not enable shared mode unless the user says multiple
agents or shared GPU resources are involved.

In shared mode, command execution requires an active task lease. File-only operations still
work normally, but `process-start` and `sync-run --command ...` must include the lease
headers.

Acquire or recover a lease:

```bash
export AP_TASK_ID="${AP_TASK_ID:-task-$(date +%s)}"
export AP_LEASE_ID="${AP_LEASE_ID:-$AP_TASK_ID-gpu}"

"$AP_BIN" mode-switch \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --mode shared \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID" \
  --ttl-seconds 300 \
  --heartbeat-seconds 30 \
  --max-renewals 20
```

Use these headers on execution requests while the lease is active:

```bash
--header 'x-agentplane-agent-mode: shared' \
--header "x-agentplane-task-id: $AP_TASK_ID" \
--header "x-agentplane-lease-id: $AP_LEASE_ID"
```

Renew during long tasks:

```bash
"$AP_BIN" lease-renew \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID"
```

Release at task boundary:

```bash
"$AP_BIN" lease-release \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID"
```

Multi-agent rules:

- use distinct `task_id`, `lease_id`, `process-id`, and preferably distinct worktree/cache
  prefixes per agent
- releasing one agent's lease must not be treated as permission to run without a lease while
  other active leases remain
- if a lease expires or is released, reacquire or renew before starting new commands
- never retry a failed `process-start` with a different `process-id` unless the user wants a
  second execution

Example shared `process-start`:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id "$AP_TASK_ID-build" \
  --cwd "$AP_REMOTE_ROOT" \
  --header 'x-agentplane-agent-mode: shared' \
  --header "x-agentplane-task-id: $AP_TASK_ID" \
  --header "x-agentplane-lease-id: $AP_LEASE_ID" \
  -- bash -lc 'make build'
```

### Sync modes

`sync-run` now has two distinct operating modes. Choose deliberately:

- default mode without `--ref`
  sync the local repo's current git delta: tracked edits, staged edits, untracked files, and tracked deletes
- snapshot mode with `--ref <branch|commit|tag>`
  sync the exact tracked file tree from that local git ref
- committed-delta mode with `--ref <target> --base-ref <base>`
  sync only the committed file changes and tracked deletes between `base..target`

Use default delta mode when:

- the user is iterating on local uncommitted edits
- the remote build directory should keep previously generated artifacts by default
- the goal is a fast edit-run loop

Use `--ref` mode when:

- the user has already committed locally
- the user just did `pull --rebase`, branch switch, reset, or other history rewrite
- the pod must match one exact committed local state
- the caller wants to delete stale remote source files that are no longer in the chosen ref

Use `--ref <target> --base-ref <base>` when:

- the remote root is already known to be at `base`
- the gateway rejects large request bodies
- the desired change is only a small committed diff on top of that base
- the caller wants committed deletes from `base..target` to propagate without sending a full snapshot

`--ref` mode reads local git objects directly. It does not depend on:

- the current checkout matching the target ref
- the local worktree being clean
- the remote container being able to reach git remotes

`--base-ref` changes only the payload strategy, not the source of truth:

- the CLI still reads local git objects
- only changed files in `base..target` are uploaded
- tracked deletes in `base..target` are sent explicitly
- the request body is usually much smaller than full `--ref` snapshot sync

### Exact mirror and preserve paths

`sync-run --ref` uses exact mirror semantics under `AP_REMOTE_ROOT`:

- tracked files from the chosen ref are written or overwritten remotely
- remote files not in that ref snapshot are deleted
- empty source directories left behind by deletions are pruned
- executable bits are corrected to match the git tree mode

`--ref --base-ref` is different:

- it is not exact mirror mode
- it assumes the remote root already matches `base-ref`
- it updates only files changed in `base..target`
- it deletes only tracked paths deleted in `base..target`
- it will not clean unrelated drift that predates the chosen base

If the remote container generates build artifacts that must survive future `--ref` syncs,
put them under stable directories and pass repeatable preserve flags:

```bash
"$AP_BIN" sync-run \
  --repo /local/project/root \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --ref HEAD \
  --preserve-path target \
  --preserve-path .cache \
  --preserve-path models
```

Preserve-path behavior is simple prefix preservation relative to `AP_REMOTE_ROOT`:

- `--preserve-path target` preserves `target/**`
- `--preserve-path build/cache` preserves only `build/cache/**`
- similar names are not preserved accidentally, for example `target` does not preserve `target-cache`

Observed behavior from local end-to-end tests:

- `--ref stable` still syncs the `stable` branch snapshot even when the current checkout is dirty
- `--dry-run --ref` previews deletes without mutating the remote side
- `--dry-run` preserves the compatibility `writes` path list and adds `write_details` for mode/checksum metadata
- `--dry-run --ref` succeeds even if the remote root does not exist yet
- empty-tree refs clear remote source files but still keep preserved directories
- files and directories with spaces in their names sync correctly
- executable bits update correctly when switching between refs
- `--ref --base-ref` uploads only the committed delta payload instead of the full target tree
- `--ref --base-ref` correctly propagates committed deletes from `base..target`
- `--checksum` verifies SHA-256 values server-side and skips identical files
- `--preserve-mode` applies exact collected Unix modes
- `--atomic-files` writes uploaded files via temp-file rename
- `--include` and `--exclude-from` filter worktree-delta and `--ref --base-ref` payload paths

`sync-push` is not a separate subcommand in the current binary; use `sync-run` for these
sync-push-style operations. `--include` and `--exclude-from` support exact relative paths,
directory prefixes, `*`, and `?`. They intentionally reject exact `--ref` mirror sync
without `--base-ref`; use `--preserve-path` when exact mirror mode must keep generated
remote subtrees.

### Recommended ref workflow

For a commit-aligned remote verification loop:

1. make and commit or rebase local changes
2. run `sync-run --ref <branch|commit>`
3. preserve remote build/cache/model directories with `--preserve-path`
4. run `process-start` or a one-shot `--command`
5. inspect logs with `process-read --follow --text`
6. return to default delta mode for quick local uncommitted edits if needed

For gateways with small request-body limits:

1. confirm which committed base the remote root already matches
2. prefer `sync-run --ref <target> --base-ref <base>` for small committed changes
3. use plain `--ref` only when you truly need exact remote realignment
4. if the delta is still too large, fall back to targeted `file-write` operations for the few changed files

## Gateway mode

This mode is for gateways and path-based reverse proxy deployments where the
AgentPlane service is behind a path like:

```text
https://gateway.example.com/workspaces/dev/agentplane
```

and direct requests need additional user-provided headers.

### Required request context

Capture these from a user-provided command, profile, or request sample:

- service root URL
- custom headers as raw `Name: value` strings

Typical source request is the workspace page:

```text
https://gateway.example.com/workspaces/dev
```

The service root is the full routed URL that reaches AgentPlane; do not infer hidden path
segments from a separate workspace URL.

The `AP_SERVER` service root and `AP_TOKEN` are **not** guaranteed to be present in the
workspace URL or the browser `curl`. If the user did not state them, ask for them; do not
guess paths and do not brute-force the token.

### First probe

Prefer the CLI first when it supports repeatable `--header 'Name: value'`. Use raw `curl`
only as a fallback for diagnosis.

CLI probe:

```bash
"$AP_BIN" health \
  --server "$AP_SERVER" \
  --connect-retries 5 \
  --connect-retry-delay-ms 3000 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2"
```

Fallback raw probe:

```bash
curl -sS -D /tmp/agentplane.headers -o /tmp/agentplane.body \
  "$AP_SERVER/healthz" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  -H 'Accept: application/json,text/plain,*/*'
```

Interpretation:

- `200 {"ok":true,...}`: gateway path is correct
- `302` to OIDC or login: required request context is missing, stale, or wrong
- `404`: wrong routed service URL

### Gateway requests

Business endpoints still need the server token in addition to any gateway-required custom
headers. Prefer the CLI with repeated `--header` flags:

```bash
"$AP_BIN" process-list \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --connect-retries 5 \
  --connect-retry-delay-ms 3000 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Use raw `curl` only when the CLI is unavailable or when you need wire-level diagnosis:

```bash
curl --compressed -sS "$AP_SERVER/v1/process/list" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  -H "$AP_HEADER_3" \
  --data '{}'
```

Observed response meanings:

- `401 {"ok":false,"error":"unauthorized"}`: gateway headers were accepted, but the server token is wrong or missing
- `422 missing field ...`: gateway headers were accepted, but the request body is malformed
- `200`: request succeeded

Use `--compressed` because many gateways gzip JSON responses.

### Recommended retry policy for Gateway

Gateway and similar enterprise gateways can have minute-scale bad windows where:

- `health` may recover before `process-start`
- a few immediate retries still fail
- the same request succeeds later without any payload change

In that case, do not spin a tight retry loop. Prefer a real interval:

- light instability: `--connect-retries 5 --connect-retry-delay-ms 3000`
- persistent gateway jitter: `--connect-retries 10 --connect-retry-delay-ms 10000`
- bad-window bridge for critical background starts: `--connect-retries 30 --connect-retry-delay-ms 20000`

Recommended long-job start pattern:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id build-1 \
  --connect-retries 30 \
  --connect-retry-delay-ms 20000 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
  -- bash -lc 'make build'
```

Use that only for idempotent `process-start` with a stable `--process-id`. Do not copy the
same retry policy to `process-write`, `file-write`, or `sync-run`.

### Gateway gateway size limit

Assume the Gateway gateway rejects or destabilizes requests and responses larger than
roughly **10 MB**.

Operate defensively:

- keep each single request and response comfortably below 10 MB
- do not send large inline file payloads through one `file-write`
- do not ask for huge log reads in one `process-read`
- do not assume gzip will save an oversized response enough to be safe

Preferred mitigations:

- for logs, poll incrementally with `after_seq`, smaller `max_bytes`, and narrow tails
- for files, read or write one file at a time and avoid very large binaries
- for sync, prefer smaller deltas and avoid bundling large generated artifacts
- for validation, ask for summaries first, then fetch more only if needed

If a needed operation is likely to exceed the gateway limit, stop trying to brute-force it
through the proxy and ask the user to help with one of these:

- run a command inside the container and paste back the result
- move a large artifact by another channel
- split the operation into smaller batches
- temporarily expose a more direct path if available

### Common request shapes

List files:

```bash
"$AP_BIN" file-list \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path . \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Equivalent diagnostic `curl`:

```bash
curl --compressed -sS "$AP_SERVER/v1/file/list" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data "{\"remote_root\":\"$AP_REMOTE_ROOT\",\"path\":\".\"}"
```

Read a file:

```bash
"$AP_BIN" file-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path README.md \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Equivalent diagnostic `curl`:

```bash
curl --compressed -sS "$AP_SERVER/v1/file/read" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data "{\"remote_root\":\"$AP_REMOTE_ROOT\",\"path\":\"README.md\"}"
```

Stat or wait for a file:

```bash
"$AP_BIN" file-stat \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \

"$AP_BIN" file-wait \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path reports/result.json \
  --min-bytes 1 \
  --stable-ms 1000 \
  --timeout-seconds 300 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Run a short command:

```bash
"$AP_BIN" sync-run \
  --repo /local/project/root \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --command 'pwd' \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Equivalent diagnostic `curl`:

```bash
curl --compressed -sS "$AP_SERVER/v1/sync-run" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data "{\"remote_root\":\"$AP_REMOTE_ROOT\",\"writes\":[],\"deletes\":[],\"command\":\"pwd\",\"timeout_seconds\":10,\"env\":null}"
```

Start a background process:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id build-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --output-bytes-limit 8388608 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
  -- /bin/sh -lc 'echo start && sleep 1 && echo done'
```

In Gateway shared mode, add the same three lease headers to execution requests in
addition to the required custom gateway headers.

Equivalent diagnostic `curl`:

```bash
curl --compressed -sS "$AP_SERVER/v1/process/start" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data "{\"remote_root\":\"$AP_REMOTE_ROOT\",\"process_id\":\"build-1\",\"command\":[\"/bin/sh\",\"-lc\",\"echo start && sleep 1 && echo done\"],\"cwd\":\"$AP_REMOTE_ROOT\",\"env\":null,\"timeout_seconds\":30,\"output_bytes_limit\":8388608,\"pipe_stdin\":false}"
```

Read process output:

```bash
"$AP_BIN" process-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id build-1 \
  --max-bytes 262144 \
  --wait-ms 30000 \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2" \
  --header "$AP_HEADER_3" \
```

Equivalent diagnostic `curl`:

```bash
curl --compressed -sS "$AP_SERVER/v1/process/read" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data '{"process_id":"build-1","after_seq":null,"max_bytes":8388608,"wait_ms":30000}'
```

In Gateway mode, prefer much smaller values than the server-side maximum. Start with:

- `process-read max_bytes`: `262144` to `1048576`
- output tailing instead of full history
- one file per request

Only increase payload size gradually if the gateway remains stable.

## Long-running jobs and reconnect safety

Use a stable `process_id` for builds, servers, benchmarks, and inference jobs.

Rules:

- retry `process-start` with the same `process_id` after disconnects
- if the response says `created:false` and `already_exists:true`, treat that as success
- use `process-get` or `process-list` to rediscover running work
- resume logs with `process-read` and the latest `next_seq`
- terminate abandoned sessions with `process-terminate`

Long startup gaps are normal. Do not assume a hang just because logs are quiet for tens of
seconds.

## Remote proxy inside the container

The container itself may also export `http_proxy` and `https_proxy`. That affects commands
you launch remotely.

When probing a localhost service inside the container, pass:

```bash
--env NO_PROXY=127.0.0.1,localhost --env no_proxy=127.0.0.1,localhost
```

and prefer:

```bash
curl --noproxy '*' http://127.0.0.1:8000/health
```

This is independent from the local shell proxy issue.

## Path and output rules

- `--cwd` for `process-start` must stay inside `AP_REMOTE_ROOT`
- file paths are relative to `AP_REMOTE_ROOT`
- use larger output budgets for verbose builds or model startup logs
- if output is truncated, continue with `after_seq` and larger `max_bytes`
- in Gateway mode, gateway transport is the tighter limit, so prefer smaller reads even
  if the server itself allows more

## Default workflow

1. Determine and state `AP_ACCESS_MODE`: `direct` or `gateway`
2. Confirm `AP_SERVER`, `AP_TOKEN`, and `AP_REMOTE_ROOT`; ask the user for any that are
   missing — do not guess tokens, ports, or paths
3. Probe `healthz`
4. If the user requested shared GPU/container usage, acquire or recover a lease before
   command execution
5. In Gateway mode, budget every request under the gateway's 10 MB transfer ceiling
6. Confirm basic read access with `process-list` or `file-list`
7. Prefer local edits as the source of truth
8. Before GPU-specific work, call `accelerator-status --kind gpu` or `gpu-status` once to establish availability; if it returns `available:false`, do not repeat GPU probes unless the user says the environment changed. If GPUs are available, use `gpu-preflight` before launch and `gpu-wait-idle` after teardown instead of hand-written JSON polling
9. Use default `sync-run` for git-delta loops, `sync-run --ref` for exact committed snapshots, and `sync-run --ref ... --base-ref ...` for low-payload committed deltas on known remote baselines
10. Use file and process APIs for targeted remote manipulation
11. For long jobs, manage them as reconnect-safe sessions and renew shared-mode leases
12. For orphan cleanup, run `process-cleanup --dry-run --text` first and send `--kill --signal TERM` only after reviewing matched PIDs
13. Release shared-mode leases at task boundary

## Safety

- keep the local repo as the only human-edited source of truth
- do not silently widen `--allow-root` or `AP_REMOTE_ROOT`
- treat tokens and custom request headers as secrets
- never guess or brute-force missing secrets or endpoint details (`AP_TOKEN`, service
  port/root). If the user did not provide them, ask. Confirming a user-supplied URL with one
  `health` probe is verification; trying multiple token values or scanning ports to discover
  the service is guessing and is not allowed
- if the gateway login state looks stale, ask for a fresh request capture instead of guessing
- if a request mutates remote state and the prior result is unknown, check `process-get`,
  `process-list`, or the target files before retrying
- if Gateway transfer size is likely to exceed the gateway limit, ask the user to
  cooperate instead of forcing a brittle oversized request
- do not proceed if `AP_ACCESS_MODE` is unknown; the agent must know whether gateway
  headers are required before touching the remote container
- do not use `--ref` blindly against remote roots that contain valuable generated data unless
  the required artifact directories are protected with `--preserve-path`
- do not use `--base-ref` unless the remote baseline is known with high confidence; if the
  remote state may have drifted, prefer exact `--ref` or explicitly inspect and repair the
  remote root first
- in shared mode, do not start command execution without active lease headers; if another
  agent's lease is still active, the container is still in shared mode
- release only your own `task_id`/`lease_id`; do not release another agent's lease unless the
  user explicitly asks
- do not use `process-cleanup` as a probe loop. If `accelerator-status` or `gpu-status`
  says `available:false`, treat that as no GPU cleanup/preflight/wait target for the
  current container session unless the user says the environment changed
- never run `process-cleanup --kill` without first reviewing a fresh dry-run result with the
  same `--match` terms

## When to inspect the implementation

If the current binary behavior is unclear and the source repo is available locally, inspect:

- `<AgentPlane repo>/src/cli/mod.rs` and subcommands in
  `<AgentPlane repo>/src/cli/*.rs`
- `<AgentPlane repo>/src/server.rs` and submodules in
  `<AgentPlane repo>/src/server/{auth.rs,error.rs,accelerator.rs,file.rs,process.rs}`
- `<AgentPlane repo>/src/protocol/mod.rs` and DTO modules in
  `<AgentPlane repo>/src/protocol/{common.rs,mode.rs,sync.rs,accelerator.rs,process.rs,file.rs}`
- `<AgentPlane repo>/tests/{git_sync,process,file,accelerator,mode,gateway,health}.rs`
- `<AgentPlane repo>/tests/common/mod.rs`

These are internal source layout paths only; external CLI/API usage and the skill workflow
above are unchanged.

Pay special attention to:

- auth handling
- reconnect-safe process semantics
- output retention and truncation
- file path validation
- timeout and server limits
