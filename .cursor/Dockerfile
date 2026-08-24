# Cloud Agent base image for GrokPtah.
# System packages only — Cursor checks out the repo separately. Do not COPY the tree.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:${PATH} \
    RUST_VERSION=1.92.0 \
    NODE_VERSION=22.14.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        sudo \
        python3 \
        xz-utils \
        build-essential \
        pkg-config \
        protobuf-compiler \
        libdbus-1-dev \
        clang \
        cmake \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Node 22 matches the Cloud Agent default; desktop lockfile is npm.
RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
    | tar -xJ -C /usr/local --strip-components=1 \
    && node --version \
    && npm --version

# Pinned to rust-toolchain.toml. rustfmt/clippy are used by hosted-service CI.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain "${RUST_VERSION}" --profile default --component rustfmt,clippy \
    && chmod -R a+w "${RUSTUP_HOME}" "${CARGO_HOME}" \
    && rustc --version \
    && cargo --version

RUN id -u ubuntu >/dev/null 2>&1 || useradd -m -s /bin/bash ubuntu \
    && usermod -aG sudo ubuntu \
    && echo "ubuntu ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/ubuntu \
    && chmod 0440 /etc/sudoers.d/ubuntu

USER ubuntu
WORKDIR /home/ubuntu
