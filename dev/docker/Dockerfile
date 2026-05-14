# syntax=docker/dockerfile:1.7

# ── Stage 0: Frontend build ─────────────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN npm ci --ignore-scripts 2>/dev/null || npm install --ignore-scripts
COPY web/ .
RUN npm run build

# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.95-slim@sha256:5021128d455987e7e7d6586bd7288fa876614821292614acbb761c21fc1ebb15 AS builder

WORKDIR /app
ARG THALA_CARGO_FEATURES="memory-postgres"

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
# Create dummy targets declared in Cargo.toml so manifest parsing succeeds.
RUN mkdir -p src benches \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs
RUN --mount=type=cache,id=thala-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=thala-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=thala-target,target=/app/target,sharing=locked \
    if [ -n "$THALA_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$THALA_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi
RUN rm -rf src benches

# 2. Copy only build-relevant source paths (avoid cache-busting on docs/tests/scripts)
COPY src/ src/
COPY benches/ benches/
COPY --from=web-builder /web/dist web/dist
COPY *.rs .
RUN touch src/main.rs
RUN --mount=type=cache,id=thala-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=thala-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=thala-target,target=/app/target,sharing=locked \
    rm -rf target/release/.fingerprint/thala-* \
           target/release/deps/thala-* \
           target/release/incremental/thala-* && \
    if [ -n "$THALA_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$THALA_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi && \
    cp target/release/thala /app/thala && \
    strip /app/thala
RUN size=$(stat -c%s /app/thala 2>/dev/null || stat -f%z /app/thala) && \
    if [ "$size" -lt 1000000 ]; then echo "ERROR: binary too small (${size} bytes), likely dummy build artifact" && exit 1; fi

# Prepare runtime directory structure and default config inline (no extra stage)
RUN mkdir -p /thala-data/.thala /thala-data/workspace && \
    printf '%s\n' \
        'workspace_dir = "/thala-data/workspace"' \
        'config_path = "/thala-data/.thala/config.toml"' \
        'api_key = ""' \
        'default_provider = "openrouter"' \
        'default_model = "anthropic/claude-sonnet-4-20250514"' \
        'default_temperature = 0.7' \
        '' \
        '[gateway]' \
        'port = 42617' \
        'host = "[::]"' \
        'allow_public_bind = true' \
        > /thala-data/.thala/config.toml && \
    chown -R 65534:65534 /thala-data

# ── Stage 2: Development Runtime (Debian) ────────────────────
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS dev

# Install essential runtime dependencies only (use docker-compose.override.yml for dev tools)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /thala-data /thala-data
COPY --from=builder /app/thala /usr/local/bin/thala
COPY LICENSE LICENSE-MIT LICENSE-APACHE NOTICE /usr/share/doc/thala/

# Overwrite minimal config with DEV template (Ollama defaults)
COPY dev/config.template.toml /thala-data/.thala/config.toml
RUN chown 65534:65534 /thala-data/.thala/config.toml

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
# Use consistent workspace path
ENV THALA_WORKSPACE=/thala-data/workspace
ENV HOME=/thala-data
# Defaults for local dev (Ollama) - matches config.template.toml
ENV PROVIDER="ollama"
ENV THALA_MODEL="llama3.2"
ENV THALA_GATEWAY_PORT=42617

# Note: API_KEY is intentionally NOT set here to avoid confusion.
# It is set in config.toml as the Ollama URL.

WORKDIR /thala-data
USER 65534:65534
EXPOSE 42617
HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["thala", "status", "--format=exit-code"]
ENTRYPOINT ["thala"]
CMD ["daemon"]

# ── Stage 3: Production Runtime (Distroless) ─────────────────
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:84fcd3c223b144b0cb6edc5ecc75641819842a9679a3a58fd6294bec47532bf7 AS release

COPY --from=builder /app/thala /usr/local/bin/thala
COPY --from=builder /thala-data /thala-data
COPY LICENSE LICENSE-MIT LICENSE-APACHE NOTICE /usr/share/doc/thala/

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
ENV THALA_WORKSPACE=/thala-data/workspace
ENV HOME=/thala-data
# Default provider and model are set in config.toml, not here,
# so config file edits are not silently overridden
#ENV PROVIDER=
ENV THALA_GATEWAY_PORT=42617

# API_KEY must be provided at runtime!

WORKDIR /thala-data
USER 65534:65534
EXPOSE 42617
HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["thala", "status", "--format=exit-code"]
ENTRYPOINT ["thala"]
CMD ["daemon"]
