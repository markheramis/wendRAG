FROM rust:1.92-bookworm

# Base OS certs for HTTPS clients; curl for the HEALTHCHECK probe.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/

# Single-stage image: the binary lives in target/ from this RUN. COPY cannot take
# files from a previous layer as its source (only from the build context), so we
# install the built binary into PATH here instead of a multi-stage COPY --from.
RUN cargo build --release \
    && install -m 755 target/release/wend-rag /usr/local/bin/wend-rag

EXPOSE 3000

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["curl", "-f", "http://localhost:3000/health"]

# curl is available in the rust:bookworm base image.
CMD ["wend-rag", "daemon"]
