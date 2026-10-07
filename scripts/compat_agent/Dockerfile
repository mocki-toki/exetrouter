FROM rust:1.88-bookworm
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
