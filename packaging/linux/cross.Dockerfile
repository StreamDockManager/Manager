FROM node:26-bookworm-slim AS node
FROM ubuntu@sha256:78d4744ead1f5b19b04d0df558ab182a5300903900755e515b2ca4d10eb2044d AS build
ARG PRODUCT=desktop
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git python3 pkg-config libssl-dev libudev-dev \
    libxdo-dev libdbus-1-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
    patchelf dpkg-dev rpm && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/ /usr/local/
ENV PATH="/root/.cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
WORKDIR /src
# Install target-native frontend dependencies before copying source; never reuse macOS node_modules
COPY package.json package-lock.json .npmrc ./
RUN if [ "$PRODUCT" != cli ]; then npm ci --no-audit --no-fund; fi
COPY . .
ARG CARGO_BUILD_JOBS
ARG CARGO_NET_OFFLINE
ARG STREAM_DOCK_MANAGER_SOURCE_REVISION
ARG STREAM_DOCK_MANAGER_SOURCE_DIRTY
ARG STREAM_DOCK_MANAGER_RELEASE_REPOSITORY
ARG STREAM_DOCK_MANAGER_PLUGINS_RELEASE_REPOSITORY
ARG STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL
ARG STREAM_DOCK_MANAGER_PACKAGE_METADATA=packaging/metadata.toml
RUN --mount=type=cache,id=sdm-linux-amd64-registry,target=/root/.cargo/registry,sharing=locked \
    --mount=type=cache,id=sdm-linux-amd64-git,target=/root/.cargo/git,sharing=locked \
    --mount=type=cache,id=sdm-linux-patched-deps,target=/src/vendor/.cache,sharing=locked \
    --mount=type=cache,id=sdm-linux-amd64-target,target=/src/target/cargo,sharing=locked \
    set -e; \
    if [ "$PRODUCT" = cli ]; then set -- --package stream-dock-manager-cli; \
    else npm run build:native; set -- --package stream-dock-manager; \
        if [ "$PRODUCT" = all ]; then set -- "$@" --package stream-dock-manager-cli; fi; fi; \
    CARGO_TARGET_DIR=/src/target/cargo python3 scripts/check_syntax.py --rust-only --release \
        --target x86_64-unknown-linux-gnu "$@" && \
    TARGET=linux-amd64 PRODUCT="$PRODUCT" python3 scripts/build.py package-target --linux-container
FROM scratch
COPY --from=build /src/target/local-installers/ /
