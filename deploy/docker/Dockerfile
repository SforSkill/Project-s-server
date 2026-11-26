# Projet S dedicated server (Linux x86-64).
# Build from the repository root:
#   docker build -f deploy/docker/Dockerfile -t projet-s-server:0.1.0-alpha .
# Only the server crate and its dependencies are compiled (no Bevy).

FROM rust:1.100-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p game-server --bin projet-s-server \
 && strip target/release/projet-s-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /opt/projet-s --shell /usr/sbin/nologin projets \
 && mkdir -p /opt/projet-s /var/lib/projet-s/replays \
 && chown projets /var/lib/projet-s/replays
WORKDIR /opt/projet-s
COPY --from=build /src/target/release/projet-s-server /opt/projet-s/projet-s-server
COPY configs /opt/projet-s/configs
COPY maps /opt/projet-s/maps
COPY deploy/server.example.toml /opt/projet-s/server.toml
USER projets
ENV PROJET_S_LOG_JSON=1 RUST_LOG=info
# 27015/udp: game. 27016/tcp: admin API (/health, /metrics; /status and /command need PROJET_S_ADMIN_TOKEN).
EXPOSE 27015/udp 27016/tcp
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/27016 && printf "GET /health HTTP/1.0\r\n\r\n" >&3 && grep -q " 200 " <&3'
ENTRYPOINT ["/opt/projet-s/projet-s-server", "--data", "/opt/projet-s", "--config", "/opt/projet-s/server.toml", "--admin", "0.0.0.0:27016", "--replays", "/var/lib/projet-s/replays"]
