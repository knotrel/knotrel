# Container image for the reference PostgreSQL feeder.
#
# Not a published release artifact. Build from integrations/postgres-feeder:
#   docker build -t knotrel-pg-feeder:example integrations/postgres-feeder
# Design and limits: docs/integrations/postgresql-feeder.md.

# Same pinned bootstrap image as examples/container/Dockerfile, with the exact
# toolchain installed.
FROM rust:slim-bookworm@sha256:f2c004e3d2d8b547e8cf7ffbc29205385f4e71d23e53e4e383065db5e9f822cd AS build
RUN rustup toolchain install 1.100.0 --profile minimal
ENV RUSTUP_TOOLCHAIN=1.100.0
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin knotrel-pg-feeder

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin feeder
COPY --from=build /src/target/release/knotrel-pg-feeder /usr/local/bin/knotrel-pg-feeder
USER 10001:10001
# No volume and no state: the feeder resumes from Knotrel's frontier and its
# replication slot (design document, assumption A5).
ENTRYPOINT ["/usr/local/bin/knotrel-pg-feeder"]
CMD ["run"]
