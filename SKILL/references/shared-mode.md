# Shared Mode Reference

Load this before switching to shared mode, adding lease headers, or using resource claims.

## Contents

- [Lease Lifecycle](#lease-lifecycle)
- [Claimed Execution](#claimed-execution)
- [Conflict Interpretation](#conflict-interpretation)
- [Validated Scenario](#validated-scenario)

## Lease Lifecycle

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

## Claimed Execution

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

Use the same headers for agent-owned file work when attribution matters:

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
```

Terminate holder processes before release unless ownership is explicitly handed off:

```bash
"$AP_BIN" process-terminate \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id "$PROCESS_ID"
```

Restore single if this workflow enabled shared:

```bash
"$AP_BIN" mode-switch \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --mode single
```

## Conflict Interpretation

- `reserved by active lease`: expected resource isolation. Wait, choose another resource
  intentionally, or ask the user.
- `process_id already exists`: expected reconnect/idempotency protection. Inspect the
  existing process; do not silently create a second process with a new id.
- lease expired/released: reservation is gone, but old processes may remain. Inspect and
  terminate or hand off before taking over.

## Validated Scenario

Run `real-subagents-shared3-910b-20260718105831-76575` used three real child agents:

- Agent A held a claimed port and completed file/process work.
- Agent B was blocked by the active port claim, then completed independent work.
- Agent C was blocked by Agent A's process id, then completed independent work.
- Main cleanup terminated the holder, released leases, restored `single`, removed the remote
  directory, and found no run-specific running process.

No P0 shared-mode implementation bug was found. The operational risk is stale leases; renew
or reacquire and inspect state before continuing.
