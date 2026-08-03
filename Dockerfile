FROM rust:1-bookworm AS build
ARG ZPL_AGENT_GIT_COMMIT=unknown
ENV ZPL_AGENT_GIT_COMMIT=${ZPL_AGENT_GIT_COMMIT}
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
RUN cargo build --locked --release

FROM build AS test
RUN rustup component add rustfmt clippy \
    && cargo fmt -- --check \
    && cargo clippy --all-targets -- -D warnings \
    && cargo test --locked

FROM scratch AS artifact
COPY --from=build /src/target/release/zpl-agent /zpl-agent

FROM debian:bookworm-slim
RUN groupadd --system zpl-agent && useradd --system --gid zpl-agent --home-dir /var/lib/zpl-agent zpl-agent \
    && install -d -o zpl-agent -g zpl-agent /var/lib/zpl-agent /etc/zpl-agent
COPY --from=build /src/target/release/zpl-agent /usr/local/bin/zpl-agent
COPY config.example.toml /etc/zpl-agent/config.toml
USER zpl-agent
EXPOSE 8080/tcp 5353/udp
VOLUME ["/var/lib/zpl-agent"]
ENTRYPOINT ["/usr/local/bin/zpl-agent"]
CMD ["--config", "/etc/zpl-agent/config.toml"]
