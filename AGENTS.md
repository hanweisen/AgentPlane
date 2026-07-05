# AGENTS.md

Project-level engineering notes for this repository.

## Scope

These instructions apply to the whole `AgentPlane` repository.

## Project Shape

This is a Rust CLI/server project.

- Main Rust crate: repository root
- CLI entrypoint: `src/cli/mod.rs`
- CLI subcommand implementations: `src/cli/*.rs`
- Server entrypoint: `src/server.rs`
- Server submodules: `src/server/{auth.rs,error.rs,accelerator.rs,file.rs,process.rs}`
- Shared protocol root: `src/protocol/mod.rs`
- Protocol DTO modules: `src/protocol/{common.rs,mode.rs,sync.rs,accelerator.rs,process.rs,file.rs}`
- Git and sync helpers: `src/git.rs`
- Agent mode and lease logic: `src/mode.rs`
- Integration/end-to-end tests:
  `tests/{git_sync,process,file,accelerator,mode,gateway,health}.rs`
- Shared test helpers: `tests/common/mod.rs`

These paths describe internal layout only; external CLI/API usage is
unchanged unless explicitly documented elsewhere.

Do not treat `dist/`, `target/`, `.cargo-target*`, or `.cargo-home*` as source unless the
task is explicitly about packaging, release artifacts, or build environment setup.

## Public Contract

- Do not store tokens, custom gateway headers, generated TLS keys, or captured request
  samples in the repository.
- Keep CLI help, protocol structs, README examples, and the paired skill aligned when
  user-visible behavior changes.
- Preserve existing behavior where possible. If a protocol change is breaking, call it out
  and update tests and docs in the same change.
- Keep changes small and close to the affected module. Avoid unrelated formatting,
  generated artifact churn, or cleanup outside the requested scope.

## Remote Operation Semantics

- `health` does not require a token; business endpoints do.
- `process-start` should be reconnect-safe when retried with the same stable
  `process-id` and identical arguments.
- Use bounded `process-read` calls and resume with `after-seq`/`next-seq` for long logs.
- In Gateway mode or routed service URLs, keep request and response payloads well
  below the gateway limit; prefer smaller log reads and committed deltas.
- Use default `sync-run` for local worktree delta loops.
- Use `sync-run --ref` only when the remote root should mirror a committed ref, and add
  repeatable `--preserve-path` values for caches or model artifacts that must survive.
- Use `sync-run --ref <target> --base-ref <base>` only when the remote root is known to
  match `base`.

## Accelerator Semantics

- Distinguish provider availability from selection results. If GPU status can see GPU 0-7
  but a caller requests GPU 99, report the selected GPU as missing; do not imply the whole
  machine has no GPU.
- Avoid agent hints that overgeneralize filtered results. Messages like "No GPU detected"
  are only valid when the provider itself is unavailable or reports no devices before
  applying user selection filters.
- Preflight blockers should name the actionable condition: requested GPU missing,
  threshold exceeded, metric unknown, or forbidden process matched.

## Validation

For ordinary Rust changes, validate from the repository root:

```bash
cd <repo-root>
cargo fmt --check
cargo test
```

For Linux container release builds from macOS, follow the README's cross-build commands
and keep `CARGO_HOME` and `CARGO_TARGET_DIR` pointed at the repo-local cache directories.

If validation cannot be run, say exactly which checks were skipped and why.
