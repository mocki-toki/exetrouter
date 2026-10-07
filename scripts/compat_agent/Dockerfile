FROM rust:1.99-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy
WORKDIR /work
COPY . /work
RUN cargo fetch --locked
ENV CARGO_NET_OFFLINE=true
ENV CARGO_HOME=/cache
ENV CARGO_BUILD_JOBS=2
ENV EXR_NO_UPDATE_CHECK=1
