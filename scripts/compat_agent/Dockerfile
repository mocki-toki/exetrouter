FROM rust:1.99-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy
WORKDIR /work
COPY . /work
RUN cargo fetch --locked
RUN useradd --uid 10001 --create-home compat \
    && chmod -R a+rX /usr/local/cargo /usr/local/rustup
USER 10001:10001
ENV HOME=/tmp/compat-home
ENV CARGO_NET_OFFLINE=true
ENV CARGO_HOME=/cache
ENV CARGO_BUILD_JOBS=2
ENV CARGO_PROFILE_DEV_DEBUG=0
ENV CARGO_PROFILE_TEST_DEBUG=0
ENV CARGO_INCREMENTAL=0
ENV EXR_NO_UPDATE_CHECK=1
