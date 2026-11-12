#!/bin/bash
# Build wprsc and wprsd in a rust:1.88.0 (Debian bookworm) container.
# Binaries end up in <repo>/dist/ and run on Debian bookworm or newer.
# Must be run from the project root (the directory containing Cargo.toml):
#   cd /path/to/wprs && ./build.sh
set -euo pipefail

REPO="$PWD"
if [ ! -f "$REPO/Cargo.toml" ]; then
  echo "error: no Cargo.toml in $REPO; run build.sh from the wprs project root" >&2
  exit 1
fi
DIST="$REPO/dist"
REGISTRY="${CARGO_HOME:-$HOME/.cargo}/registry"
PROFILE=release-lto

if command -v podman >/dev/null 2>&1; then
  ENGINE=podman
else
  ENGINE=docker
fi

# Rootless podman maps container root to the invoking user, so the output is
# already ours. For docker (rootful), chown the output back at the end.
CHOWN=""
if [ "$ENGINE" = docker ]; then
  CHOWN="$(id -u):$(id -g)"
fi

# podman refuses to bind-mount paths that don't exist.
mkdir -p "$REPO/target/container" "$REGISTRY" "$DIST"

"$ENGINE" run --rm \
  -v "$REPO":/wprs:z \
  -v "$REGISTRY":/usr/local/cargo/registry:z \
  -v "$REPO/target/container":/wprs/target/container:z \
  -v "$DIST":/wprs/dist:z \
  -e CARGO_TARGET_DIR=/wprs/target/container \
  -w /wprs \
  rust:1.88.0 bash -c "
    set -euo pipefail
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends libxkbcommon-dev libwayland-dev pkg-config >/dev/null
    cargo build --profile=$PROFILE --bin wprsc --bin wprsd
    cp target/container/$PROFILE/wprsc target/container/$PROFILE/wprsd dist/
    if [ -n '$CHOWN' ]; then chown -R '$CHOWN' target/container dist /usr/local/cargo/registry; fi
  "

echo "Built: $DIST/wprsc $DIST/wprsd"
