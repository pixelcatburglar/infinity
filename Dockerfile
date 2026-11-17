# syntax=docker/dockerfile:1.7
# Release-only build (no debug target, per AGENT.md).

FROM rust:1.97-slim-bookworm AS build
ARG INFINITY_BUILD_COMMIT=dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    INFINITY_BUILD_COMMIT="$INFINITY_BUILD_COMMIT" cargo build --release --locked \
 && mkdir -p /out \
 && find target/release -maxdepth 1 -type f -perm -u+x -exec cp {} /out/ \;

# `docker build --target test .` runs the unit tests in release mode.
FROM build AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo test --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /usr/local/bin/
COPY --from=build /src/crates/server/lua /crates/server/lua
