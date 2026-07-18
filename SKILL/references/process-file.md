# Process And File Reference

Load this when exact process/file commands are needed.

## Contents

- [Process Choice](#process-choice)
- [File Commands](#file-commands)
- [Cleanup](#cleanup)
- [Run Aggregation](#run-aggregation)

## Process Choice

Use `process-run` for short commands whose remote exit code should become the local exit
code. The client automatically selects WebSocket output or HTTP cursor reads:

```bash
"$AP_BIN" process-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id build-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 1800 \
  --output-bytes-limit 8388608 \
  --tail-on-error 65536 \
  -- bash -lc 'make build'
```

Use `process-start` for long-running producers, services, and benchmarks:

```bash
"$AP_BIN" process-start \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id server-1 \
  --cwd "$AP_REMOTE_ROOT" \
  --output-bytes-limit 8388608 \
  --save-output-path logs/server-1.log \
  -- bash -lc './server'
```

Read logs incrementally:

```bash
"$AP_BIN" process-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id server-1 \
  --max-bytes 262144 \
  --wait-ms 30000
```

For one blocking follow operation, add `--follow --text`. The client tries WebSocket and resumes
through HTTP with the last `next_seq` if the upgrade or stream fails. SOCKS/custom-CA/insecure-TLS
profiles automatically use HTTP. For transport diagnosis only, set the client environment variable
`AP_PROCESS_TRANSPORT=auto|http|websocket`; do not add transport selection to routine commands.

Resume with the prior response's `next_seq`:

```bash
"$AP_BIN" process-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id server-1 \
  --after-seq "$NEXT_SEQ" \
  --max-bytes 262144
```

Terminate:

```bash
"$AP_BIN" process-terminate \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --process-id server-1
```

## File Commands

Read text:

```bash
"$AP_BIN" file-read \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path logs/server-1.log \
  --text
```

Upload with checksum:

```bash
SHA256="$(shasum -a 256 "$LOCAL_PATH" | awk '{print $1}')"
"$AP_BIN" file-upload \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path "$REMOTE_PATH" \
  --from-local "$LOCAL_PATH" \
  --create-parents \
  --checksum "$SHA256" \
  --chunk-size 1048576
```

Upload automatically prefers raw `application/octet-stream` chunks and falls back to JSON/base64
for compatibility. On an upstream HTML or AgentPlane 413, lower `--chunk-size`; do not retry the
larger base64 form. For transport diagnosis only, set the client environment variable
`AP_UPLOAD_TRANSPORT=auto|json|binary`; do not add transport selection to routine commands.

Wait for producer output:

```bash
"$AP_BIN" file-wait \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --path "$REMOTE_PATH" \
  --min-bytes 1 \
  --stable-ms 500 \
  --timeout-seconds 60 \
  --process-id "$PRODUCER_PROCESS_ID"
```

Delete or stat:

```bash
"$AP_BIN" file-stat --server "$AP_SERVER" --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" --path "$REMOTE_PATH"
"$AP_BIN" file-delete --server "$AP_SERVER" --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" --path "$REMOTE_PATH"
```

## Cleanup

Always dry-run with the exact same match before kill:

```bash
"$AP_BIN" process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|evalscope' \
  --dry-run \
  --accelerator-summary npu \
  --text

"$AP_BIN" process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|evalscope' \
  --kill \
  --signal TERM
```

Require `verified:true` after kill. For device memory release, wait briefly and run another
dry-run with `--accelerator-summary`.

## Run Aggregation

Use `--run-id` on related `process-start`/`process-run` calls, then:

```bash
"$AP_BIN" run-show --profile "$PROFILE" --rebuild "$RUN_ID"
```
