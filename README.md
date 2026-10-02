# patok

Autonomous build loop driving agent CLIs. See the `patok-spec` repository for the specification.

## Build

```sh
cargo build --release -p patok
```

The toolchain is pinned in `rust-toolchain.toml` (current stable). The MSRV is declared in the
workspace `Cargo.toml` (`rust-version`) and follows "latest stable minus two"; CI builds both.
