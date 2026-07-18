# Accelerator Reference

Load this before GPU/NPU launch, preflight, wait-idle, or cleanup.

## Status

```bash
"$AP_BIN" accelerator-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --kind gpu \
  --json

"$AP_BIN" accelerator-status \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --kind npu \
  --json
```

Legacy aliases may exist:

```bash
"$AP_BIN" gpu-status --server "$AP_SERVER" --token "$AP_TOKEN" --json
"$AP_BIN" npu-status --server "$AP_SERVER" --token "$AP_TOKEN" --json
```

Interpretation:

- `available:false`: provider cannot be queried or reports no devices. Do not loop probes
  unless the environment changed.
- requested device missing: selection problem, not whole-machine unavailability.
- process lists are advisory; use cleanup dry-run before killing.

## Preflight

```bash
"$AP_BIN" accelerator-preflight \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --kind gpu \
  --gpus 0 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --forbid-match 'xgl|vllm|nsys|evalscope'
```

For GPU-only aliases:

```bash
"$AP_BIN" gpu-preflight \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --gpus 0-7 \
  --max-memory-mib 256 \
  --max-util-percent 5 \
  --stable-seconds 10 \
  --timeout-seconds 180 \
  --forbid-match 'xgl|vllm|nsys|evalscope'
```

## Wait Idle

```bash
"$AP_BIN" accelerator-wait-idle \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --kind npu \
  --stable-seconds 10 \
  --timeout-seconds 180
```

`gpu-wait-idle` loops until selected GPUs stay below thresholds. On timeout, use the last
snapshot to identify the blocking PID/command.

## Cleanup With Occupancy

Use cleanup dry-run summaries to connect matched commands to device occupancy:

```bash
"$AP_BIN" process-cleanup \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --match 'xgl|vllm|mooncake|msprof|nsys' \
  --dry-run \
  --accelerator-summary gpu \
  --text
```

Treat `available:false` as unknown occupancy, not zero occupancy.

