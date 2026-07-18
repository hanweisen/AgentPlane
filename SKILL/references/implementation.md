# Implementation Reference

Load this only when current binary behavior is unclear and the source repo is available.

## Source Layout

- CLI entrypoint: `src/cli/mod.rs`
- CLI subcommands: `src/cli/*.rs`
- Server entrypoint: `src/server.rs`
- Server modules: `src/server/{auth.rs,error.rs,accelerator.rs,file.rs,process.rs}`
- Protocol DTOs: `src/protocol/{common.rs,mode.rs,sync.rs,accelerator.rs,process.rs,file.rs}`
- Git/sync helpers: `src/git.rs`
- Agent mode and leases: `src/mode.rs`
- Tests: `tests/{git_sync,process,file,accelerator,mode,gateway,health}.rs`
- Shared test helpers: `tests/common/mod.rs`

## Risk Areas

- auth handling and gateway headers
- reconnect-safe `process-start`
- output retention, cursor expiry, and truncation
- file path validation under `AP_REMOTE_ROOT`
- sync delete semantics with `--ref` / `--base-ref`
- accelerator provider availability versus selected-device filtering
- shared-mode lease expiry and resource claim release semantics

## Validation

For ordinary Rust changes:

```bash
cargo fmt --check
cargo test
```

Do not treat `dist/`, `.cargo-target*`, or `.cargo-home*` as source unless the task is
explicitly about release artifacts.

