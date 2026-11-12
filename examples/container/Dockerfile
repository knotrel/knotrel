# Example container image for knotrel-server in source-managed mode.
#
# Not a published release artifact. Build from the repository root:
#   docker build -f examples/container/Dockerfile -t knotrel-server:example .
# Usage, trust and recovery assumptions: docs/deployment.md.

# The versioned 1.100.0 image is not yet available. Pin the bootstrap image by
# digest and install the exact compiler without unnecessary development tools.
FROM rust:slim-bookworm@sha256:f2c004e3d2d8b547e8cf7ffbc29205385f4e71d23e53e4e383065db5e9f822cd AS build
RUN rustup toolchain install 1.100.0 --profile minimal
ENV RUSTUP_TOOLCHAIN=1.100.0
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Release profile, default features only: test-support is never enabled here.
RUN cargo build --release --locked -p knotrel-server --bin knotrel-server

FROM debian:bookworm-slim
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin knotrel
COPY --from=build /src/target/release/knotrel-server /usr/local/bin/knotrel-server
USER 10001:10001
# The listener must bind a non-loopback address inside the container, which
# requires the explicit opt-in. Publish the port on the host to 127.0.0.1 or a
# trusted network only: the API has no authentication, authorization or TLS.
ENV KNOTREL_MODE=source_managed \
    KNOTREL_ADDR=0.0.0.0:8080 \
    KNOTREL_ALLOW_NON_LOOPBACK=true
EXPOSE 8080
# Liveness only. /health returns 200 in every synchronization state; use /ready
# for traffic routing. Uses bash /dev/tcp so the image needs no HTTP client.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /health HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200 '"]
# No volume: the graph is held only in process memory and is rebuilt by the
# feeder after every restart.
ENTRYPOINT ["/usr/local/bin/knotrel-server"]
