---
name: agentplane
description: Operate a remote machine, container, or shared workspace with AgentPlane for health checks, process sessions, file transfer/editing, sync-run loops, accelerator preflight/cleanup, gateway routing, and optional shared-mode lease-backed resource claims. Use when the user wants Codex to run or validate work remotely through an AgentPlane direct server or gateway URL.
---

# AgentPlane

Use this skill to operate a remote container through `agentplane`. Prefer local edits as the
source of truth, then use AgentPlane for remote execution, files, logs, sync, accelerator
checks, cleanup, and shared resource coordination.

## Inputs

Confirm these before mutating remote state:

- `AP_BIN`: local `agentplane` binary or path
- `AP_ACCESS_MODE`: `direct` or `gateway`
- `AP_SERVER`: direct server URL or routed gateway service root
- `AP_TOKEN`: server token
- `AP_REMOTE_ROOT`: absolute root inside the remote container
- gateway headers when `AP_ACCESS_MODE=gateway`

Never guess tokens, ports, roots, or gateway headers. One `health` probe against a
user-supplied URL is verification; scanning ports or trying common token values is guessing.
Treat tokens, custom headers, and browser cookies as secrets.

Prefer a local profile/env file for repeated commands:

```bash
"$AP_BIN" --profile /tmp/agentplane.env process-list
```

The profile may provide server/token/root/headers but not the executable path. If `AP_BIN`
is absent, resolve an existing local `agentplane` binary explicitly before mutating remote
state; do not rebuild only to discover a path.

## Mode Choice

Use `direct` when `AP_SERVER` reaches the AgentPlane service directly. Use `gateway` when the
service is behind a routed URL that needs browser/session headers. In gateway mode, keep
payloads well below gateway limits; load `references/gateway.md` before using curl
diagnostics, custom headers, or large transfer workarounds.

Default execution mode is `single`. Enable `shared` only when multiple agents or shared
resources are involved; load `references/shared-mode.md` before switching modes or using
resource claims.

## Default Workflow

1. State the access mode: `direct` or `gateway`.
2. Confirm `AP_SERVER`, `AP_TOKEN`, `AP_REMOTE_ROOT`, and required gateway headers.
3. Probe `health`; direct health does not require a token, business endpoints do.
4. Confirm basic access with `process-list` or `file-list`.
5. For GPU/NPU work, run one accelerator status/preflight check; load
   `references/accelerator.md`.
6. For remote command work, choose `process-run` for short commands whose exit code should
   propagate locally, or `process-start` for long-lived work.
7. For local edit/run loops against an initialized remote workspace, use `sync-run`; use
   `sync-init` or `sync-run --ref HEAD` for fresh remote roots. Load `references/sync.md`
   before `--ref`, `--base-ref`, preserve paths, or gateway-safe chunking.
8. Use bounded `process-read` calls with `after-seq`/`next-seq` for long logs.
9. For orphan cleanup, run `process-cleanup --dry-run --text` first; use `--kill --signal
   TERM` only after reviewing the matched PIDs.
10. Release shared-mode leases and restore `single` if this workflow enabled `shared`.

## Process And File Rules

Load `references/process-file.md` for exact command shapes, file upload/read/delete/wait
patterns, tail-on-error, run-id aggregation, and cleanup examples.

Core rules:

- `process-start` must use a stable `process-id`; retries with identical args are
  reconnect-safe.
- If `created:false` and `already_exists:true`, treat it as a successful reconnect.
- Do not change to a new `process-id` unless a second execution is intended.
- Use `process-read --max-bytes` and resume with `after-seq`; avoid huge one-shot reads,
  especially through a gateway.
- Use `file-upload` for larger files; avoid oversized `file-write` payloads.
- File paths are relative to `AP_REMOTE_ROOT`; `--cwd` must stay inside `AP_REMOTE_ROOT`.
- Use `file-wait --process-id <producer>` when waiting for producer output.
- Use `process-run --tail-on-error` for commands where a failing tail matters.

## Cleanup Rules

`process-cleanup` is dry-run by default unless `--kill` is present. Actual signaling
requires `--kill --signal TERM` or `--kill --signal KILL`. Matching is case-insensitive
substring matching against process commands, with `|` separating alternatives. Avoid broad
matches such as `python`, `bash`, or `server` unless the user confirms the dry-run report.

Kill requests use bounded server-side reconfirmation by default. Require `verified: true`
before treating cleanup as successful; use `--no-reconfirm` only when explicitly choosing
not to wait. `elapsed_seconds` is available for reliable process-age comparisons.

For an AgentPlane-managed service, use `process-terminate` for controlled shutdown. It sends
SIGTERM first and escalates to SIGKILL only after its bounded grace window. `process-cleanup`
is broad orphan cleanup; if a launcher and application both match, it signals descendants
before ancestors, but it is not a service lifecycle API.

Server-launched processes and `sync-run --command` run in sessions isolated from AgentPlane,
so application process-group signals cannot terminate the control server. Use
`--kill-tree-on-terminate` when later termination must cover the whole isolated process group.

Use `--accelerator-summary gpu|npu` on dry-runs to attach matched per-PID device index/name
and used/total memory. Treat `available:false` as unknown occupancy, not zero occupancy.
After a kill, run a fresh dry-run summary when device-memory release must be confirmed.

## Accelerator Rules

Load `references/accelerator.md` before GPU/NPU launch or cleanup.

- Distinguish provider availability from selection results.
- If provider status can see devices but the selected GPU/NPU is missing, report the
  selection as missing, not "no accelerator detected".
- Prefer `accelerator-preflight` / `gpu-preflight` before launch and
  `accelerator-wait-idle` / `gpu-wait-idle` after teardown.
- If status returns `available:false`, do not repeat status/preflight/wait loops unless the
  user says the environment changed.

## Shared Mode Rules

Load `references/shared-mode.md` before switching to `shared`, using lease headers, or
claiming resources.

In shared mode, `process-start` and `sync-run --command ...` require an active lease and
the three lease headers. File-only operations can run without a lease, but use headers for
agent-owned file work when attribution matters.

Use explicit claims such as `--claim gpu:0` or `--claim port:6006`. Claims protect resources
only while the lease is active. If a lease expires or is released, the reservation
disappears but old processes are not auto-terminated; inspect state before taking over.

Treat `reserved by active lease` as correct isolation and `process_id already exists` as the
reconnect/idempotency guard. Do not bypass either silently.

## Gateway Rules

Load `references/gateway.md` for header capture, curl diagnostics, retry policy, and common
request shapes.

- Keep each request/response comfortably below gateway limits.
- Prefer chunked upload and incremental log reads.
- If gateway login state is stale, ask for a fresh request capture instead of guessing.
- Do not paste or store browser cookies in the repo.

## Sync Rules

Load `references/sync.md` before non-default sync modes.

- Default `sync-run` is best for local uncommitted edit/run loops after the remote
  workspace has been initialized.
- Use `sync-init` or `sync-run --ref HEAD` for fresh remote roots.
- Use `sync-run --ref <target>` only when the remote root should mirror a committed ref.
- Use `sync-run --ref <target> --base-ref <base>` only when the remote root is known to
  match `base`.
- Add repeatable `--preserve-path` values for caches or model artifacts that must survive.

## Safety

- Keep local source as the human-edited truth.
- Do not widen `--allow-root` or `AP_REMOTE_ROOT` silently.
- Check remote state before retrying a mutating request with unknown outcome.
- In shared mode, release only your own `task_id`/`lease_id` unless the user asks otherwise.
- Never run `process-cleanup --kill` without a fresh dry-run with the same match terms.
- If current binary behavior is unclear and the source repo is available, load
  `references/implementation.md`.
