#!/bin/sh
# Install official ExetRouter binaries or build the locked Git source.
set -eu
repo='mocki-toki/exetrouter'
role='client'
prefix="${HOME:?HOME is required}/.local"
version='latest'
source_build=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --role|--prefix|--version)
            [ "$#" -ge 2 ] || { printf '%s\n' 'Missing option value' >&2; exit 2; }
            case "$1" in --role) role=$2 ;; --prefix) prefix=$2 ;; --version) version=$2 ;; esac
            shift 2 ;;
        --from-source) source_build=true; shift ;;
        --help|-h)
            printf '%s\n' 'Usage: install.sh [--role client|server|both] [--prefix ROOT] [--version vX.Y.Z] [--from-source]'
            exit 0 ;;
        *) printf '%s\n' 'Unknown installer option' >&2; exit 2 ;;
    esac
done
case "$role" in client|server|both) ;; *) printf '%s\n' 'Invalid role' >&2; exit 2 ;; esac
case "$prefix" in /*) ;; *) printf '%s\n' 'Installation prefix must be absolute' >&2; exit 2 ;; esac
if [ "$version" != latest ]; then
    case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) printf '%s\n' 'Version must be a release tag such as v0.1.0' >&2; exit 2 ;; esac
    case "$version" in *[!a-zA-Z0-9.-]*) printf '%s\n' 'Invalid release tag' >&2; exit 2 ;; esac
fi
case "$(uname -s)" in Linux) platform=unknown-linux-gnu ;; Darwin) platform=apple-darwin ;; *) printf '%s\n' 'Only Linux and macOS are supported' >&2; exit 2 ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; arm64|aarch64) arch=aarch64 ;; *) printf '%s\n' 'Only x86_64 and ARM64 are supported' >&2; exit 2 ;; esac
if [ "$source_build" = true ]; then
    command -v cargo >/dev/null 2>&1 || { printf '%s\n' 'Install Rust 1.88 or newer and a C compiler, then retry.' >&2; exit 1; }
    set -- cargo install --locked --force --git "https://github.com/$repo" --root "$prefix"
    if [ "$version" != latest ]; then set -- "$@" --tag "$version"; fi
    case "$role" in client) set -- "$@" --bin exr ;; server) set -- "$@" --bin exrd ;; both) set -- "$@" --bin exr --bin exrd ;; esac
    "$@"
else
    command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required' >&2; exit 1; }
    umask 077
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT HUP INT TERM
    artifact="exetrouter-$arch-$platform.tar.gz"
    if [ "$version" = latest ]; then
        base="https://github.com/$repo/releases/latest/download"
    else
        base="https://github.com/$repo/releases/download/$version"
    fi
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$base/$artifact" -o "$work/$artifact" || {
        printf '%s\n' 'Release unavailable. Use --from-source or select an existing --version.' >&2; exit 1;
    }
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$base/SHA256SUMS" -o "$work/SHA256SUMS"
    awk -v artifact="$artifact" '$2 == artifact { print }' "$work/SHA256SUMS" > "$work/checksum"
    [ "$(wc -l < "$work/checksum" | tr -d ' ')" = 1 ] || { printf '%s\n' 'Release checksum missing or ambiguous' >&2; exit 1; }
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$work" && sha256sum -c checksum)
    else
        (cd "$work" && shasum -a 256 -c checksum)
    fi
    # The release archive is flat. Refuse traversal and unexpected archive paths.
    tar -tzf "$work/$artifact" > "$work/files"
    while IFS= read -r entry; do
        case "$entry" in exr|exrd|LICENSE) ;; *) printf '%s\n' 'Unexpected release archive path' >&2; exit 1 ;; esac
    done < "$work/files"
    tar -xzf "$work/$artifact" -C "$work"
    case "$role" in client) binaries=exr ;; server) binaries=exrd ;; both) binaries='exr exrd' ;; esac
    for binary in $binaries; do
        [ -f "$work/$binary" ] && [ ! -L "$work/$binary" ] || { printf '%s\n' 'Release binary is missing or not a regular file' >&2; exit 1; }
        chmod u+x "$work/$binary"
        "$work/$binary" --version >/dev/null 2>&1 || {
            printf 'Prebuilt %s cannot run on this system. Use --from-source (Linux client releases require glibc 2.39+).\n' "$binary" >&2; exit 1;
        }
    done
    mkdir -p "$prefix/bin"
    for binary in $binaries; do
        temporary=$(mktemp "$prefix/bin/.$binary.new.XXXXXX")
        if ! install -m 755 "$work/$binary" "$temporary"; then rm -f "$temporary"; exit 1; fi
        if ! mv -f "$temporary" "$prefix/bin/$binary"; then rm -f "$temporary"; exit 1; fi
    done
fi
printf 'Installed %s to %s/bin. Add this directory to PATH if needed.\n' "$role" "$prefix"
printf '%s\n' 'Connection, state, services and SSH settings were not changed.'
