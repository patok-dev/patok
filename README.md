# patok

Autonomous build loop driving agent CLIs.

## Install

Supported platforms: macOS (arm64, x86_64) and Linux (x86_64).

```sh
curl -fsSL https://github.com/patok-dev/patok/releases/latest/download/install.sh | sh
```

The script downloads the latest release archive, verifies its SHA-256 against
the release `checksums`, and installs the `patok` binary to `~/.local/bin`.
Make sure that directory is on your `PATH`.

## Uninstall

Remove the binary and the per-user data directory:

```sh
curl -fsSL https://github.com/patok-dev/patok/releases/latest/download/install.sh | sh -s -- --clean
```
