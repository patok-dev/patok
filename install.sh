#!/bin/sh
# patok install script (macOS arm64/x86_64, Linux x86_64).
#
# Resolves the latest GitHub release, downloads the platform archive
# (patok-<target>.tar.gz) and the `checksums` asset, verifies the archive's
# SHA-256, and only then extracts and installs the `patok` binary.
#
# Options:
#   --clean            first delete the installed binary and the per-user data
#                      directory ($XDG_DATA_HOME/patok or ~/.local/share/patok)
#
# Linux arm64 is not supported and is rejected.
#
# Environment:
#   PATOK_INSTALL_DIR  install directory (default: $HOME/.local/bin)
#   PATOK_REPO         GitHub repository as owner/name (default: patok/patok)

set -eu

REPO="${PATOK_REPO:-patok/patok}"
INSTALL_DIR="${PATOK_INSTALL_DIR:-$HOME/.local/bin}"

err() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"
}

need curl
need tar
need uname
need mktemp

clean=0
for arg in "$@"; do
    case "$arg" in
        --clean) clean=1 ;;
        -h | --help)
            printf 'usage: install.sh [--clean]\n'
            printf '  --clean  first delete the installed binary and the patok data directory\n'
            exit 0
            ;;
        *) err "unknown option: $arg (usage: install.sh [--clean])" ;;
    esac
done

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) target="x86_64-unknown-linux-musl" ;;
    Linux/aarch64 | Linux/arm64) err "Linux arm64 is not supported; patok is only released for Linux x86_64 and macOS (arm64/x86_64)" ;;
    Darwin/arm64 | Darwin/aarch64) target="aarch64-apple-darwin" ;;
    Darwin/x86_64) target="x86_64-apple-darwin" ;;
    *) err "unsupported platform: $(uname -s)/$(uname -m)" ;;
esac

if [ "$clean" = 1 ]; then
    if [ -n "${XDG_DATA_HOME:-}" ]; then
        data_dir="$XDG_DATA_HOME/patok"
    elif [ -n "${HOME:-}" ]; then
        data_dir="$HOME/.local/share/patok"
    else
        err "cannot determine the patok data directory: neither XDG_DATA_HOME nor HOME is set"
    fi
    printf 'Cleaning: removing %s/patok and %s\n' "$INSTALL_DIR" "$data_dir"
    rm -f "$INSTALL_DIR/patok" || err "could not remove $INSTALL_DIR/patok"
    rm -rf "$data_dir" || err "could not remove $data_dir"
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    err "required command not found: sha256sum or shasum"
fi

tmp="$(mktemp -d)" || err "could not create a temporary directory"
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Resolving latest release of %s...\n' "$REPO"
curl -fsSL -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$REPO/releases/latest" -o "$tmp/release.json" \
    || err "could not query the latest release of $REPO"
tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp/release.json" | head -n 1)"
[ -n "$tag" ] || err "could not determine the latest release tag"

archive="patok-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

printf 'Downloading %s (%s)...\n' "$archive" "$tag"
curl -fsSL "$base/$archive" -o "$tmp/$archive" || err "failed to download $archive"
curl -fsSL "$base/checksums" -o "$tmp/checksums" || err "failed to download checksums"

# Exact filename match: lines are "<sha256>  <filename>".
expected="$(awk -v f="$archive" '$2 == f { print $1; exit }' "$tmp/checksums")"
[ -n "$expected" ] || err "no checksum listed for $archive"
actual="$(sha256_of "$tmp/$archive")"
if [ "$expected" != "$actual" ]; then
    err "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
fi

mkdir "$tmp/extract"
tar -xzf "$tmp/$archive" -C "$tmp/extract" || err "failed to extract $archive"
[ -f "$tmp/extract/patok" ] || err "archive does not contain a patok binary"

mkdir -p "$INSTALL_DIR" || err "could not create $INSTALL_DIR"
cp "$tmp/extract/patok" "$INSTALL_DIR/.patok.new.$$" || err "could not write to $INSTALL_DIR"
chmod 755 "$INSTALL_DIR/.patok.new.$$"
mv -f "$INSTALL_DIR/.patok.new.$$" "$INSTALL_DIR/patok"

printf 'Installed patok %s to %s/patok\n' "$tag" "$INSTALL_DIR"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) printf 'Note: %s is not on your PATH.\n' "$INSTALL_DIR" ;;
esac
