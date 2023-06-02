FROM rust:slim as base

ENV TINI_VERSION v0.19.0
ADD https://github.com/krallin/tini/releases/download/${TINI_VERSION}/tini-static /tini
RUN chmod +x /tini


RUN apt-get update \
    && apt-get upgrade -y \
    && rm -rf /var/lib/apt/lists/*

FROM base as builder


RUN apt-get update \
    && apt-get install -y pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*


WORKDIR /usr/src/just-one-init
RUN cargo init --bin .
COPY Cargo.toml Cargo.lock ./
RUN cargo build
COPY . .

RUN cargo build --bin just-one-init --release

FROM base

COPY --from=builder /usr/src/just-one-init/target/release/just-one-init /usr/local/bin/just-one-init
RUN chmod a+rx /usr/local/bin/just-one-init

RUN groupadd -g 568 nonroot
RUN useradd -u 568 -g 568 nonroot
USER nonroot

ENTRYPOINT ["/tini", "--", "just-one-init", "--"]