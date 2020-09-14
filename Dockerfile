FROM ubuntu:20.04

WORKDIR /code

RUN \
    apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gcc \
        make \
        m4 \
        llvm \
        clang && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.lock /code/Cargo.lock
COPY Cargo.toml /code/Cargo.toml

# Hack to make Cargo download and cache dependencies
RUN \
    mkdir benches && \
    echo "" > benches/sloth.rs && \
    mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    /root/.cargo/bin/cargo build --release && \
    rm -rf src

COPY src /code/src

RUN \
    # TODO: Next line is a workaround for https://github.com/rust-lang/cargo/issues/7969
    touch src/main.rs && \
#    /root/.cargo/bin/cargo test --release && \
    /root/.cargo/bin/cargo build --release && \
    mv target/release/subspace-core-rust subspace-core-rust && \
    rm -rf target

FROM ubuntu:20.04

COPY --from=0 /code/subspace-core-rust /subspace-core-rust

ENV RUST_LOG=info

ENTRYPOINT ["/subspace-core-rust"]
