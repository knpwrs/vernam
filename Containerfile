# Everything building Vernam needs, so the host needs nothing but podman.
# Debian trixie is the release the workspace is verified with: its protoc
# is the 3.21 line the build scripts run, and its libclang is what
# bindgen links. The well-known-type protos (google/protobuf/*.proto)
# come with libprotobuf-dev, not protobuf-compiler.
FROM debian:trixie

# ffmpeg and a hunspell dictionary are for the tests, which the README's
# `./build-linux.sh test --workspace` runs: without them the media and spell
# check tests skip themselves, quietly running less than the native path.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config protobuf-compiler libprotobuf-dev clang libclang-dev \
        python3 curl ca-certificates git \
        libfontconfig1-dev libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev \
        libwayland-dev libvulkan-dev libudev-dev libasound2-dev \
        ffmpeg hunspell-en-us \
    && rm -rf /var/lib/apt/lists/*

# Under /opt so a volume can keep cargo's registry between runs (the
# libsignal and RingRTC git checkouts are hundreds of megabytes; without
# the volume, every run fetches them again). The toolchain itself is part
# of the image: the repo pins no toolchain file, so the one baked in is
# the one used, and it is the one the workspace is verified with.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH="/opt/cargo/bin:${PATH}"
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path --default-toolchain 1.99.1
