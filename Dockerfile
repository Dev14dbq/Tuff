# tuff, a Minecraft Java 26.3 server. Game data is not in the image:
# mount cache/mcdata and the server folder.
FROM rust:1.96.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p mc-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/tuff /usr/local/bin/tuff
WORKDIR /data
EXPOSE 25565
ENTRYPOINT ["tuff"]
CMD ["--bind", "0.0.0.0"]
