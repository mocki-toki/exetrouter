# Native multi-stage build; no runtime Rust toolchain or build credentials.
FROM rust:1.90-bookworm AS build
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY scripts/install.sh ./scripts/install.sh
RUN --mount=type=cache,target=/source/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --locked --release --bins && mkdir -p /out && cp target/release/exrd target/release/exr /out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/exrd /usr/local/bin/exrd
COPY --from=build /out/exr /usr/local/bin/exr
USER 10001:10002
ENTRYPOINT ["/usr/local/bin/exrd"]
CMD ["serve", "--allow-container-listen"]
