# Sync Reference

Load this before `sync-init` or `sync-run`, especially when using `--ref`, `--base-ref`,
preserve paths, or gateway-safe chunking.

## Contents

- [Default Worktree Delta](#default-worktree-delta)
- [Fresh Remote Root](#fresh-remote-root)
- [Exact Committed Snapshot](#exact-committed-snapshot)
- [Committed Delta](#committed-delta)
- [Gateway-Safe Sync](#gateway-safe-sync)

## Default Worktree Delta

Use default `sync-run` for local edit/run loops with uncommitted changes:

```bash
"$AP_BIN" sync-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT" \
  --command 'cargo test'
```

Default sync preserves generated remote artifacts better than exact mirror modes.

Use this mode when the remote root already contains the workspace you want to update.
It creates the target root if needed, but it only transfers local delta content, so an
empty fresh root will not contain unchanged tracked files after the run.

## Fresh Remote Root

For first-time remote setup or an empty validation root, use `sync-init` to mirror the
current worktree, or use `sync-run --ref HEAD` when you want a committed snapshot:

```bash
"$AP_BIN" sync-init \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT"

"$AP_BIN" process-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --remote-root "$AP_REMOTE_ROOT" \
  --process-id "$RUN_ID-sync-check" \
  --cwd "$AP_REMOTE_ROOT" \
  --timeout-seconds 60 \
  -- bash -lc 'test -f Cargo.toml'

"$AP_BIN" sync-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT" \
  --ref HEAD \
  --command 'test -f Cargo.toml'
```

Use a unique remote subdirectory only when you need an isolated validation root.

## Exact Committed Snapshot

Use `--ref` only when the remote root should mirror a committed ref:

```bash
"$AP_BIN" sync-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT" \
  --ref main \
  --preserve-path target \
  --preserve-path .venv \
  --command 'cargo test'
```

Behavior:

- writes tracked files from the ref
- deletes remote tracked files missing from that ref
- prunes empty source directories
- fixes executable bits
- ignores dirty local checkout content

## Committed Delta

Use `--ref <target> --base-ref <base>` only when the remote root is known to match `base`:

```bash
"$AP_BIN" sync-run \
  --server "$AP_SERVER" \
  --token "$AP_TOKEN" \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT" \
  --ref feature \
  --base-ref main \
  --command 'cargo test'
```

This sends only `base..target` changes and committed deletes. It does not repair unrelated
remote drift.

## Gateway-Safe Sync

For gateway mode, keep payloads small:

```bash
"$AP_BIN" sync-run \
  --profile /tmp/agentplane.env \
  --repo "$LOCAL_REPO" \
  --remote-root "$AP_REMOTE_ROOT" \
  --upload-chunk-size 1048576 \
  --command 'pytest -q'
```

Use `--dry-run` to inspect planned writes/deletes before exact mirror syncs.
