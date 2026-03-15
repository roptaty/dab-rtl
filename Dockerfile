# ── Build stage ──────────────────────────────────────────────────────────── #
FROM rust:1.94-slim-bookworm AS builder

RUN echo 'deb http://deb.debian.org/debian bookworm main contrib non-free' > /etc/apt/sources.list && \
    apt-get update && apt-get install -y --no-install-recommends \
    librtlsdr-dev \
    libasound2-dev \
    libpulse-dev \
    libfdk-aac-dev \
    pkg-config \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

RUN cargo build --release -p dab-rtl

# ── Runtime stage ─────────────────────────────────────────────────────────── #
FROM debian:bookworm-slim

RUN echo 'deb http://deb.debian.org/debian bookworm main contrib non-free' > /etc/apt/sources.list && \
    apt-get update && apt-get install -y --no-install-recommends \
    librtlsdr0 \
    libasound2 \
    libpulse0 \
    libfdk-aac2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/dab-rtl /usr/local/bin/dab-rtl

ENTRYPOINT ["/usr/local/bin/dab-rtl"]
CMD ["--help"]
