#!/usr/bin/env bash
set -euo pipefail

action=$1
target=$2
case "$target" in
  x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc) ;;
  *) echo "unsupported server target: $target" >&2; exit 1 ;;
esac

case "$action" in
  prepare)
    rustup toolchain install nightly-2026-11-14 --profile minimal --target "$target"
    rustup default nightly-2026-11-14
    ;;
  build)
    cargo build --locked --release --bin zess --target "$target"
    ;;
  package)
    binary=$3
    if [ "$target" = x86_64-unknown-linux-gnu ]; then
      mkdir -p patchelf licenses
      curl -fsSL https://github.com/NixOS/patchelf/releases/download/0.19.3/patchelf-0.19.3-x86_64.tar.gz | tar -xz -C patchelf
      curl -fsSL https://raw.githubusercontent.com/NixOS/patchelf/0.19.3/COPYING -o licenses/patchelf-GPL-3.0
      curl -fsSL https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT -o licenses/musl-COPYRIGHT
      cp /usr/share/doc/libc6/copyright licenses/glibc-copyright
      cp /usr/share/doc/libgcc-s1/copyright licenses/gcc-copyright
      ZESS_DEBIAN_LICENSES=1 python scripts/server-release.py package "$target" "$binary" dist patchelf/bin/patchelf licenses
    else
      python scripts/server-release.py package "$target" "$binary" dist
    fi
    ;;
  *) echo "unknown server build action: $action" >&2; exit 1 ;;
esac
