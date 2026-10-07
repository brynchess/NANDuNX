# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim AS web-builder
WORKDIR /build

COPY package.json package-lock.json ./
RUN npm ci

COPY index.html ./
COPY src ./src
RUN npm run build:web

# Keep the builder aligned with the workspace MSRV and locked Tauri dependencies.
FROM rust:1.90-bookworm AS rust-builder
WORKDIR /build

# Cargo needs every workspace manifest even though this image builds only the
# headless adapter.
COPY Cargo.toml Cargo.lock ./
COPY crates/nandunx-core/Cargo.toml crates/nandunx-core/Cargo.toml
COPY apps/nandunx-web/Cargo.toml apps/nandunx-web/Cargo.toml
COPY src-tauri/Cargo.toml src-tauri/Cargo.toml
COPY crates ./crates
COPY apps ./apps
# Cargo parses every workspace member before selecting -p nandunx-web.
COPY src-tauri ./src-tauri
RUN cargo build --locked --release -p nandunx-web

FROM debian:bookworm-slim AS runtime

WORKDIR /opt/nandunx
COPY --from=web-builder /build/dist /opt/nandunx/web
COPY --from=rust-builder /build/target/release/nandunx-web /usr/local/bin/nandunx-web

# These paths stay writable through the state bind mount and the tmpfs defined
# in Compose. Keeping the image filesystem read-only is part of the runtime
# hardening.
RUN install -d -m 0700 /var/lib/nandunx/uploads /run/nandunx \
    && chmod 1777 /tmp

ENV NANDUNX_BIND=127.0.0.1:4321 \
    NANDUNX_EDITION=docker \
    NANDUNX_WEB_ROOT=/opt/nandunx/web \
    NANDUNX_ARTIFACT_DIR=/var/lib/nandunx/uploads \
    NANDUNX_LAST_RUN_LOG=/var/lib/nandunx/last-run.log

EXPOSE 4321
ENTRYPOINT ["/usr/local/bin/nandunx-web"]
