# syntax=docker/dockerfile:1
#
# tuff, a Minecraft: Java Edition 26.3 server.
#
# The image holds no Mojang files. Game data is extracted once from your own
# server.jar (`cargo xtask extract-data`, see README "Running it") and mounted
# at /data/cache/mcdata; the server folder (world, server.properties, lists,
# logs) is the /data volume. Details: docs/admin/docker.md.

FROM rust:1.96.1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p mc-server \
    && install -D target/release/tuff /out/tuff \
    && mkdir -p /out/data

# glibc, libgcc and CA certificates, no shell; runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/tuff /usr/local/bin/tuff
COPY --from=build --chown=65532:65532 /out/data /data
WORKDIR /data
VOLUME /data
EXPOSE 25565
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/tuff"]
# server-ip defaults to 127.0.0.1, which is unreachable from outside a
# container.
CMD ["--bind", "0.0.0.0", "--nogui"]
