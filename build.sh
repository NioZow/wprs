#!/bin/bash
podman run --rm \
  -v /home/user/internet/wprs:/wprs:z \
  -v /home/user/.cargo/registry:/usr/local/cargo/registry:z \
  -v /home/user/internet/wprs/target:/wprs/target:z \
  -w /wprs \
  rust:1.88.0 bash -c \
    "apt-get update -qq && apt-get install -y -qq libxkbcommon-dev libwayland-dev pkg-config 2>&1 | tail -3 && cargo build --profile=release-lto"
