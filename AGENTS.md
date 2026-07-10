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

## MiniMax Subagent Validation Discipline

When asking a MiniMax subagent to validate AgentPlane locally, give it a narrow, explicit
script and require evidence for each step. The subagent must not broaden scope, redesign
the feature, edit unrelated files, rebuild release artifacts, push code, or start remote
machine work unless the prompt explicitly asks for that.

If a MiniMax subagent cannot complete a requested step immediately because a command,
dependency, port, permission, or environment value is missing, it must stop and report the
exact blocker, command output, and the last successful step. It should not guess tokens,
scan ports, change ports repeatedly, or keep trying unrelated alternatives.

For upload or sync validation, the subagent must record:

- server command, PID, and log paths
- exact client command
- effective `--chunk-size` or `--upload-chunk-size`
- HTTP status or CLI exit code
- SHA-256 before and after transfer when file contents matter
- whether any 413 response is JSON from AgentPlane or HTML from an upstream gateway/proxy

## Release Builds From macOS

Build release artifacts from macOS with the rustup toolchain binaries and Zig wrappers.
Keep build caches repo-local and untracked. Do not commit `dist/`, `.cargo-home*`, or
`.cargo-target*` unless the task explicitly asks for release artifacts.

Prerequisites:

```bash
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin
command -v /opt/homebrew/bin/zig
```

Create Zig compiler wrappers before Linux builds. The wrappers deliberately remove
`--target=<rust-triple>` arguments that `cc-rs`, `ring`, or `aws-lc-sys` may add, because
Zig expects targets like `aarch64-linux-gnu` instead of `aarch64-unknown-linux-gnu`.

```bash
cat > /tmp/zigcc-x86_64-linux-gnu <<'EOF'
#!/bin/bash
args=()
for arg in "$@"; do
  case "$arg" in
    --target=x86_64-unknown-linux-gnu|-target=x86_64-unknown-linux-gnu) ;;
    *) args+=("$arg") ;;
  esac
done
exec /opt/homebrew/bin/zig cc -target x86_64-linux-gnu "${args[@]}"
EOF

cat > /tmp/zigcxx-x86_64-linux-gnu <<'EOF'
#!/bin/bash
args=()
for arg in "$@"; do
  case "$arg" in
    --target=x86_64-unknown-linux-gnu|-target=x86_64-unknown-linux-gnu) ;;
    *) args+=("$arg") ;;
  esac
done
exec /opt/homebrew/bin/zig c++ -target x86_64-linux-gnu "${args[@]}"
EOF

cat > /tmp/zigcc-aarch64-linux-gnu <<'EOF'
#!/bin/bash
args=()
for arg in "$@"; do
  case "$arg" in
    --target=aarch64-unknown-linux-gnu|-target=aarch64-unknown-linux-gnu) ;;
    *) args+=("$arg") ;;
  esac
done
exec /opt/homebrew/bin/zig cc -target aarch64-linux-gnu "${args[@]}"
EOF

cat > /tmp/zigcxx-aarch64-linux-gnu <<'EOF'
#!/bin/bash
args=()
for arg in "$@"; do
  case "$arg" in
    --target=aarch64-unknown-linux-gnu|-target=aarch64-unknown-linux-gnu) ;;
    *) args+=("$arg") ;;
  esac
done
exec /opt/homebrew/bin/zig c++ -target aarch64-linux-gnu "${args[@]}"
EOF

cat > /tmp/zigar <<'EOF'
#!/bin/sh
exec /opt/homebrew/bin/zig ar "$@"
EOF

chmod +x /tmp/zigcc-x86_64-linux-gnu /tmp/zigcxx-x86_64-linux-gnu \
  /tmp/zigcc-aarch64-linux-gnu /tmp/zigcxx-aarch64-linux-gnu /tmp/zigar
```

Build all three targets:

```bash
CARGO_HOME=$PWD/.cargo-home-local \
CARGO_TARGET_DIR=$PWD/.cargo-target-linux-x86_64 \
RUSTC=/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/tmp/zigcc-x86_64-linux-gnu \
CC_x86_64_unknown_linux_gnu=/tmp/zigcc-x86_64-linux-gnu \
CXX_x86_64_unknown_linux_gnu=/tmp/zigcxx-x86_64-linux-gnu \
AR_x86_64_unknown_linux_gnu=/tmp/zigar \
/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo \
  build --release --target x86_64-unknown-linux-gnu

CARGO_HOME=$PWD/.cargo-home-local \
CARGO_TARGET_DIR=$PWD/.cargo-target-linux-aarch64 \
RUSTC=/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=/tmp/zigcc-aarch64-linux-gnu \
CC_aarch64_unknown_linux_gnu=/tmp/zigcc-aarch64-linux-gnu \
CXX_aarch64_unknown_linux_gnu=/tmp/zigcxx-aarch64-linux-gnu \
AR_aarch64_unknown_linux_gnu=/tmp/zigar \
/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo \
  build --release --target aarch64-unknown-linux-gnu

CARGO_HOME=$PWD/.cargo-home-local \
CARGO_TARGET_DIR=$PWD/.cargo-target-macos \
RUSTC=/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc \
/Users/hanweisen/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo \
  build --release --target aarch64-apple-darwin
```

Package the artifacts:

```bash
rm -rf dist/agentplane-linux-x86_64 dist/agentplane-linux-aarch64 dist/agentplane-macos-arm64
mkdir -p dist/agentplane-linux-x86_64 dist/agentplane-linux-aarch64 dist/agentplane-macos-arm64
cp .cargo-target-linux-x86_64/x86_64-unknown-linux-gnu/release/agentplane \
  dist/agentplane-linux-x86_64/agentplane
cp .cargo-target-linux-aarch64/aarch64-unknown-linux-gnu/release/agentplane \
  dist/agentplane-linux-aarch64/agentplane
cp .cargo-target-macos/aarch64-apple-darwin/release/agentplane \
  dist/agentplane-macos-arm64/agentplane
cp README.md dist/agentplane-linux-x86_64/README.md
cp README.md dist/agentplane-linux-aarch64/README.md
cp README.md dist/agentplane-macos-arm64/README.md
chmod 755 dist/agentplane-linux-x86_64/agentplane \
  dist/agentplane-linux-aarch64/agentplane \
  dist/agentplane-macos-arm64/agentplane
tar -C dist -czf dist/agentplane-linux-x86_64.tar.gz agentplane-linux-x86_64
tar -C dist -czf dist/agentplane-linux-aarch64.tar.gz agentplane-linux-aarch64
tar -C dist -czf dist/agentplane-macos-arm64.tar.gz agentplane-macos-arm64
```

Verify release artifacts:

```bash
file dist/agentplane-linux-x86_64/agentplane \
  dist/agentplane-linux-aarch64/agentplane \
  dist/agentplane-macos-arm64/agentplane
shasum -a 256 dist/agentplane-*.tar.gz
tar -tzf dist/agentplane-linux-x86_64.tar.gz | sed -n '1,5p'
tar -tzf dist/agentplane-linux-aarch64.tar.gz | sed -n '1,5p'
tar -tzf dist/agentplane-macos-arm64.tar.gz | sed -n '1,5p'
```
