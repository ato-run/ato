# Capsuled Engine - Production Dockerfile
# Multi-stage build for minimal runtime image
#
# Build context: capsuled/ root
# Build: docker build -f engine/Dockerfile -t capsuled-engine:latest .

# NOTE: This Dockerfile is built by CI as ghcr.io/<owner>/gumball-engine:<tag>.
# When run via docker-compose.yml (context: ../), the build context is onescluster/ root.
# So the path to engine is /workspace/capsuled/engine
# Avoid "optimizing" COPY patterns unless you re-validate workspace builds and migrations.

# ============================================
# Stage 1: Build Stage (Rust)
# ============================================
# Note: Using Rust 1.85+ for edition2024 support (required by spdx crate)
FROM rust:1.85-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    capnproto \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# Copy entire workspace (build context is repo root)
COPY capsuled capsuled

# Navigate to engine within capsuled subdirectory
WORKDIR /workspace/capsuled/engine

# Build release binary
RUN cargo build --release --bin capsuled-engine

# ============================================
# Stage 2: Runtime Stage
# ============================================
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies + Docker CLI (for DooD pattern)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    netcat-openbsd \
    gnupg \
    && rm -rf /var/lib/apt/lists/*

# Add Docker's official GPG key and repository
RUN install -m 0755 -d /etc/apt/keyrings && \
    curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc && \
    chmod a+r /etc/apt/keyrings/docker.asc && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" > /etc/apt/sources.list.d/docker.list && \
    apt-get update && \
    apt-get install -y docker-ce-cli && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r capsuled && useradd -r -g capsuled capsuled

# Create necessary directories
RUN mkdir -p /var/lib/capsuled /var/log/capsuled /tmp/capsuled/logs /tmp/capsuled/keys \
    && chown -R capsuled:capsuled /var/lib/capsuled /var/log/capsuled /tmp/capsuled

# Copy binary from builder
COPY --from=builder /workspace/capsuled/engine/target/release/capsuled-engine /usr/local/bin/

# Copy migrations
COPY --from=builder /workspace/capsuled/engine/migrations /var/lib/capsuled/migrations

# Set environment
ENV RUST_LOG=info
ENV CAPSULED_DATA_DIR=/var/lib/capsuled
ENV CAPSULED_LOG_DIR=/var/log/capsuled

# NOTE: Running as root for Docker socket access (DooD pattern)
# In production, use docker group or rootless Docker
# USER capsuled

# IMPORTANT: If you switch to non-root, you must also adjust runtime deployment:
# - docker socket permissions (/var/run/docker.sock)
# - container runtime strategy (DooD vs rootless)

# Expose ports
# 50051: gRPC
# 4500: HTTP API
# 4501: Egress Proxy
EXPOSE 50051 4500 4501

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:4500/health || exit 1

# Default command
ENTRYPOINT ["capsuled-engine"]
CMD ["--port", "4500"]
