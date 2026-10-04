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
# Host gateway and optional compatibility helpers are shipped with the same image.
COPY deploy/docker/update-host deploy/docker/exrd-host /usr/local/share/exetrouter/
ARG EXETROUTER_VERSION=development
ARG EXETROUTER_REVISION=unknown
LABEL org.opencontainers.image.source="https://github.com/mocki-toki/exetrouter" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="$EXETROUTER_VERSION" \
      org.opencontainers.image.revision="$EXETROUTER_REVISION" \
      io.exetrouter.schema-version="8"
USER 10001:10002
ENTRYPOINT ["/usr/local/bin/exrd"]
CMD ["serve", "--allow-container-listen"]
