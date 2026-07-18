# Gateway Reference

Load this for routed service URLs, custom headers, browser request captures, curl
diagnostics, retries, or gateway transfer limits.

## Required Context

Ask for:

- routed service root URL
- representative browser `curl` request
- required custom headers as raw `Name: value` strings
- evidence that unauthenticated requests redirect to login

Do not guess gateway headers or store browser cookies in the repo.

## First Probe

```bash
"$AP_BIN" health \
  --server "$AP_SERVER" \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2"
```

Diagnostic curl:

```bash
curl -sS -D /tmp/agentplane.headers -o /tmp/agentplane.body \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  "$AP_SERVER/health"
```

Interpretation:

- `200 {"ok":true,...}`: route and headers work.
- `302` to login/OIDC: request context is missing, stale, or wrong.
- `404`: routed service URL is wrong.

## Business Requests

```bash
"$AP_BIN" process-list \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --header "$AP_HEADER_1" \
  --header "$AP_HEADER_2"
```

Diagnostic curl:

```bash
curl --compressed -sS "$AP_SERVER/v1/process/list" \
  -X POST \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $AP_TOKEN" \
  -H "$AP_HEADER_1" \
  -H "$AP_HEADER_2" \
  --data '{}'
```

Interpretation:

- `401 {"ok":false,"error":"unauthorized"}`: gateway headers worked; token is wrong/missing.
- `422 missing field`: gateway headers worked; request body is malformed.
- `200`: request succeeded.

## Retry Policy

Use retry knobs only for safe/reconnect-safe requests:

- light instability: `--connect-retries 5 --connect-retry-delay-ms 3000`
- persistent gateway jitter: `--connect-retries 10 --connect-retry-delay-ms 10000`
- critical background start during a bad bridge window:
  `--connect-retries 30 --connect-retry-delay-ms 20000`

`process-start` is reconnect-safe only with the same stable `process-id` and identical args.

## Size Limits

Keep each request/response comfortably below gateway limits:

- use `file-upload --chunk-size <BYTES> --resume` for large files
- use `sync-run --upload-chunk-size <BYTES>` for sync writes
- read logs incrementally with `after_seq` and small `max_bytes`
- fetch summaries first, then fetch details only if needed

If the operation is too large, ask the user to provide another channel or split the work.

