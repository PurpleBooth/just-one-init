FROM ubuntu:latest as base

# Ensure base image is up to date
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get upgrade -y \
    && rm -rf /var/lib/apt/lists/*


# Install tini
ENV TINI_VERSION v0.19.0
ADD https://github.com/krallin/tini/releases/download/${TINI_VERSION}/tini-static /tini
RUN chmod +x /tini


FROM base as builder

# Install rust
ENV CARGO_HOME="/cargo"
ENV RUSTUP_HOME="/rustup"
ENV PATH="$CARGO_HOME/bin:${PATH}"
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y \
      build-essential \
      curl \
      git  \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p \
      "$CARGO_HOME" \
      "$RUSTUP_HOME" \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile complete --no-modify-path

# Install build dependencies
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Configure Cargo & Rust
ENV RUST_BACKTRACE=1
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Cache rust dependencies
WORKDIR /usr/src/just-one-init
RUN cargo init --bin .
COPY Cargo.toml Cargo.lock ./
RUN cargo build && cargo build --release

# Build
COPY . .
RUN cargo build --bin just-one-init --release

FROM base
# Install built app
COPY --from=builder /usr/src/just-one-init/target/release/just-one-init /usr/local/bin/just-one-init
RUN chmod a+rx /usr/local/bin/just-one-init

# Run as non-root
RUN groupadd -g 568 nonroot
RUN useradd -u 568 -g 568 nonroot
USER nonroot

# Congigure entrypoit
ENTRYPOINT ["/tini", "--", "just-one-init", "--"]