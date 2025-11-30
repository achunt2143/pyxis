# Multi-stage Dockerfile for GeckoOS
# Build stage: compile everything
# Runtime stage: minimal environment with compiled artifacts

FROM ubuntu:22.04 AS builder

# Set environment variables
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/home/builder/.rustup \
    CARGO_HOME=/home/builder/.cargo \
    PATH=/home/builder/.cargo/bin:$PATH

# Install build dependencies
RUN apt-get update && apt-get install -y \
    # Build essentials
    build-essential \
    cmake \
    pkg-config \
    git \
    curl \
    wget \
    # Rust and language tools
    rustc \
    cargo \
    # Node.js and npm
    nodejs \
    npm \
    # Graphics and display
    libdrm-dev \
    libgbm-dev \
    libgl1-mesa-dev \
    libxkbcommon-dev \
    # Input and wayland
    libinput-dev \
    libwayland-dev \
    wayland-protocols \
    # System libraries
    libudev-dev \
    libasound2-dev \
    libxcb1-dev \
    libxcb-render0-dev \
    libxcb-shape0-dev \
    libxcb-xfixes0-dev \
    && rm -rf /var/lib/apt/lists/*

# Create builder user
RUN useradd -m -s /bin/bash builder && \
    mkdir -p /home/builder/.cargo && \
    chown -R builder:builder /home/builder

WORKDIR /build
COPY --chown=builder:builder . .

USER builder

# Build Rust components
RUN cargo build --release 2>&1

# Build JavaScript components
RUN npm ci && \
    npm run build 2>&1

# ============================================================================
# Runtime stage: minimal image with compiled artifacts
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

# Install runtime dependencies only
RUN apt-get update && apt-get install -y \
    # Runtime essentials
    ca-certificates \
    # Graphics
    libdrm2 \
    libgbm1 \
    libgl1-mesa-glx \
    libxkbcommon0 \
    # Input and wayland
    libinput10 \
    libwayland-client0 \
    libwayland-server0 \
    # System libraries
    libudev1 \
    libasound2 \
    libxcb1 \
    libxcb-render0 \
    libxcb-shape0 \
    libxcb-xfixes0 \
    # Runtime for node/rust
    libssl3 \
    zlib1g \
    # Utilities
    curl \
    wget \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for runtime
RUN useradd -m -s /bin/bash geckouser

WORKDIR /opt/geckoOS

# Copy compiled artifacts from builder
COPY --from=builder --chown=geckouser:geckouser /build/target/release /opt/geckoOS/bin
COPY --from=builder --chown=geckouser:geckouser /build/dist /opt/geckoOS/static
COPY --from=builder --chown=geckouser:geckouser /build/scripts /opt/geckoOS/scripts

# Make scripts executable
RUN chmod +x /opt/geckoOS/scripts/*.sh

# Set up environment
ENV PATH=/opt/geckoOS/bin:$PATH \
    GECKOOS_HOME=/opt/geckoOS

USER geckouser

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD ls /opt/geckoOS/bin || exit 1

# Default command
CMD ["/bin/bash"]
