#!/bin/sh
# Builds Vernam in a podman container: the host needs nothing but podman
# (rootless is fine), not Rust or any system library. The repo is
# bind-mounted in, so builds stay incremental and the binary lands in
# target/linux/release/vernam, owned by you (under rootless podman, the
# container's root is your user).
#
# Extra words go to cargo in the container instead of the default build:
#   ./build-linux.sh test --workspace
set -eu

cd "$(dirname "$0")"

# Cached after the first run; .containerignore keeps target/ and .git
# out of the context.
podman build -q --security-opt label=disable -t vernam-build -f Containerfile .

if [ "$#" -eq 0 ]; then
    set -- build --release
fi

# CARGO_TARGET_DIR keeps the container's build out of target/release, so
# it can sit beside a macOS checkout's builds without the two churning
# each other's artifacts, and OUTPUT_DIR moves RingRTC's prebuilt WebRTC
# fetch with it (.cargo/config.toml fixes it to target/webrtc; the
# environment wins over that). Dropping :Z and adding label=disable
# keeps podman from relabeling the checkout or giving the container
# SELinux categories: a :Z mount relabels every file in it with the
# container's own categories, and any other container sharing the
# directory with different categories then loses access. The cargo
# volume keeps the registry between runs, so only the first build
# downloads.
exec podman run --rm \
    --security-opt label=disable \
    --env CARGO_TARGET_DIR=/vernam/target/linux \
    --env OUTPUT_DIR=/vernam/target/linux/webrtc \
    --volume "$(pwd):/vernam" \
    --volume vernam-cargo:/opt/cargo \
    --workdir /vernam \
    vernam-build \
    cargo "$@"
