FROM rust:1.95-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu \
 && rustup component add clippy rustfmt
WORKDIR /work
