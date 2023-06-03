FROM ubuntu:latest as base

# Ensure base image is up to date
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get upgrade --yes \
    && rm --verbose --recursive --force  \
      /var/lib/apt/lists/*

FROM base as builder

# Install rust
ENV CARGO_HOME="/cargo"
ENV RUSTUP_HOME="/rustup"
ENV PATH="$CARGO_HOME/bin:${PATH}"
ENV DEBIAN_FRONTEND=noninteractive
RUN bash -c 'if [ "$(uname -m)" == "x86_64" ] ; then echo x86_64-unknown-linux-musl > /tmp/target.txt ; else echo aarch64-unknown-linux-musl > /tmp/target.txt ; fi'
RUN apt-get update \
    && apt-get install --yes \
      musl \
      musl-dev \
      musl-tools \
      build-essential \
      curl \
      git  \
    && rm --verbose --recursive --force \
      /var/lib/apt/lists/* \
    && mkdir -p \
      "$CARGO_HOME" \
      "$RUSTUP_HOME" \
    && curl --proto '=https' --tlsv1.2 --silent --show-error --fail \
      https://sh.rustup.rs \
    | sh \
      -s \
      -- \
      -y \
      --default-toolchain stable \
      --profile complete \
      --no-modify-path
RUN rustup target add "$( cat /tmp/target.txt )"

# Install build dependencies
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --yes \
      pkg-config  \
      libssl-dev \
    && rm --verbose --recursive --force /var/lib/apt/lists/*

# Configure Cargo & Rust
ENV RUST_BACKTRACE=1
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true



# Cache rust dependencies
WORKDIR /usr/src/just-one-init
RUN cargo init --bin .
COPY Cargo.toml Cargo.lock ./
RUN cargo build \
    --target="$( cat /tmp/target.txt )" \
    --bin just-one-init

# Build
COPY . .
RUN cargo install \
    --target="$( cat /tmp/target.txt )" \
    --path . \
    --bin just-one-init \
    --root=/usr/local

FROM base
# Install built app
COPY --from=builder \
    /usr/local/bin/just-one-init \
    /usr/local/bin/just-one-init
RUN chmod --verbose a+rx /usr/local/bin/just-one-init

# Run as non-root
RUN groupadd \
    --gid 568 \
    nonroot
RUN useradd \
    --uid 568 \
    --gid 568 \
    nonroot
USER nonroot
RUN just-one-init --help