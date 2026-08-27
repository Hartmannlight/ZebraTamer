FROM rust:1-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS build
ARG ZPL_AGENT_GIT_COMMIT=unknown
ENV ZPL_AGENT_GIT_COMMIT=${ZPL_AGENT_GIT_COMMIT}
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
RUN cargo build --locked --release

FROM scratch AS artifact
COPY --from=build /src/target/release/zpl-agent /zpl-agent

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
RUN groupadd --system zpl-agent && useradd --system --gid zpl-agent --home-dir /var/lib/zpl-agent zpl-agent \
    && install -d -o zpl-agent -g zpl-agent /var/lib/zpl-agent /etc/zpl-agent
COPY --from=build /src/target/release/zpl-agent /usr/local/bin/zpl-agent
COPY config.example.toml /etc/zpl-agent/config.toml
USER zpl-agent
EXPOSE 8080/tcp 5353/udp
VOLUME ["/var/lib/zpl-agent"]
ENTRYPOINT ["/usr/local/bin/zpl-agent"]
CMD ["--config", "/etc/zpl-agent/config.toml"]
