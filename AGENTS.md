# AGENTS.md

Before pushing any commit, every CI gate must pass locally on this machine. CI (`.github/workflows/ci.yml`) runs the same gates on both the pinned stable toolchain (`rust-toolchain.toml`, currently 1.98.1) and the MSRV toolchain (1.96, the `rust-version` in `Cargo.toml`), so local validation must cover both. Run all commands from the repository root.

## Format gate

Runs on the pinned toolchain only — CI's MSRV leg installs no rustfmt.

```sh
cargo fmt --all --check
```

## Lint gate

Pinned toolchain, then the MSRV toolchain if installed:

```sh
cargo clippy --all-targets -- -D warnings
rustup run 1.96 cargo clippy --all-targets -- -D warnings
```

## Test gate

Pinned toolchain, then the MSRV toolchain if installed:

```sh
cargo test --workspace
rustup run 1.96 cargo test --workspace
```

Run the MSRV variants whenever the 1.96 toolchain is installed. If it is missing, install it with:

```sh
rustup toolchain install 1.96 --component clippy
```

## Release build smoke check

Pinned toolchain, host target:

```sh
cargo build --release -p patok
target/release/patok --version
```

This is the host-target simplification of CI's release matrix, which cross-compiles for musl and Apple targets; the cross-compilation and static-linkage checks are CI-only. Locally, `--version` printing `patok 0.0.0` is the expected pass: the source version is pinned to 0.0.0 and the release job rewrites it from the tag, so 0.0.0 from a local build means the binary runs correctly.

## Install script gate

CI's `install-script` job syntax-checks `install.sh`, lints it with shellcheck, and runs the offline test harness behind stubbed `curl`/`uname` (no network):

```sh
sh -n install.sh
sh scripts/test-install.sh
```

`shellcheck install.sh scripts/test-install.sh` is also part of the gate; run it locally when shellcheck is installed — CI enforces it either way.

## Rule

All gates must pass with zero warnings and zero failing tests before any commit is pushed. Any rustfmt diff, clippy warning, failing test, or non-running binary blocks the push.
