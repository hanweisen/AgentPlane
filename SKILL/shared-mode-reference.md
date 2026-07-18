# Shared mode reference

This file is optional detail for the concise shared-mode section. Load it when exact
commands or conflict validation are needed.

## Lease lifecycle

```bash
"$AP_BIN" mode-get \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN"

export AP_TASK_ID="${AP_TASK_ID:-task-$(date +%s)}"
export AP_LEASE_ID="${AP_LEASE_ID:-$AP_TASK_ID-lease}"

"$AP_BIN" mode-switch \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --mode shared \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID" \
  --ttl-seconds 300 \
  --heartbeat-seconds 30 \
  --max-renewals 20

"$AP_BIN" lease-renew \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID"

"$AP_BIN" lease-release \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --task-id "$AP_TASK_ID" \
  --lease-id "$AP_LEASE_ID"
```

Execution requests in shared mode use:

```bash
--header 'x-agentplane-agent-mode: shared' \
--header "x-agentplane-task-id: $AP_TASK_ID" \
--header "x-agentplane-lease-id: $AP_LEASE_ID"
```

Example:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id "$AP_TASK_ID-build" \
  --cwd "$AP_REMOTE_ROOT" \
  --claim port:6006 \
  --header 'x-agentplane-agent-mode: shared' \
  --header "x-agentplane-task-id: $AP_TASK_ID" \
  --header "x-agentplane-lease-id: $AP_LEASE_ID" \
  -- bash -lc 'make build'
```

## File and cleanup commands

Use the same lease headers for agent-owned file work when attribution matters:

```bash
"$AP_BIN" file-upload \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --header 'x-agentplane-agent-mode: shared' \
  --header "x-agentplane-task-id: $AP_TASK_ID" \
  --header "x-agentplane-lease-id: $AP_LEASE_ID" \
  --path "$REMOTE_PATH" \
  --from-local "$LOCAL_PATH" \
  --create-parents \
  --checksum "$SHA256"

"$AP_BIN" file-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --header 'x-agentplane-agent-mode: shared' \
  --header "x-agentplane-task-id: $AP_TASK_ID" \
  --header "x-agentplane-lease-id: $AP_LEASE_ID" \
  --path "$REMOTE_PATH" \
  --text
```

Terminate holder processes before release unless ownership is explicitly handed off:

```bash
"$AP_BIN" process-terminate \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id "$PROCESS_ID"
```

Remove temporary remote files or directories with a bounded command:

```bash
"$AP_BIN" process-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id "$AP_TASK_ID-cleanup" \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 30 \
  --env "AP_CLEAN_PATH=$REMOTE_RELATIVE_PATH" \
  -- bash -lc 'rm -rf "$AP_CLEAN_PATH" && test ! -e "$AP_CLEAN_PATH"'
```

If you created shared mode for this workflow, restore single after release:

```bash
"$AP_BIN" mode-switch \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --mode single
```

## Conflict interpretation

- `reserved by active lease`: expected resource isolation. Wait, choose another resource
  intentionally, or ask the user.
- `process_id already exists`: expected reconnect/idempotency protection. Inspect the
  existing process; do not silently create a second process with a new id.
- lease expired/released: reservation is gone, but old processes may remain. Inspect and
  terminate or hand off them before taking over.

## Validated 910B scenario

Run `real-subagents-shared3-910b-20260718105831-76575` used three real child agents on one
service:

- Agent A held a claimed port, uploaded/read back a file, and ran an independent process.
- Agent B was blocked by the active port claim, then completed its own file/process work.
- Agent C was blocked by Agent A's process id, then completed its own file/process work.
- The main agent terminated the holder, released all leases, restored `single`, removed the
  remote test directory, and confirmed no run-specific process remained running.

No P0 shared-mode implementation bug was found. The operational risk is stale leases, so
long-running agents must renew or reacquire and inspect state before continuing.

Staged-doc validation run `staged-doc-worker-910b-20260718124832-10559` confirmed the
concise section correctly routes an agent to this reference. The run completed mode-get,
mode-switch, claimed holder execution, file upload/read/cmp, lease-renew, process terminate,
lease-release, single-mode restore, and remote cleanup. The only gap found was missing exact
file/terminate/cleanup command shapes, now covered above.

## Source of truth

- `src/mode.rs`
- `src/server/auth.rs`
- `src/server/process.rs`
- `src/server/file.rs`
- `src/cli/mod.rs`
- `tests/mode.rs`
- 910B evidence: `/tmp/real-subagents-shared3-910b-20260718105831-76575.*`
