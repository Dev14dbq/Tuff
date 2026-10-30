# syntax=docker/dockerfile:1
#
# tuff, a Minecraft: Java Edition 26.3 server.
#
# The image holds no Mojang files and needs no Java. On the first start tuff
# downloads server.jar from Mojang into /data/cache, reads the vanilla data
# pack out of it, and takes the generated id tables (tuff-reports-26.3.zip)
# from /data or from the Tuff release. /data is the server folder: world,
# server.properties, lists, logs. Details: docs/admin/docker.md.

# Pinned by digest; Dependabot (.github/dependabot.yml) proposes updates.
FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p mc-server \
    && install -D target/release/tuff /out/tuff \
    && mkdir -p /out/data

# glibc, libgcc and CA certificates, no shell; runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
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
