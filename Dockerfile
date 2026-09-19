FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
ARG ZPL_AGENT_GIT_COMMIT=unknown
ARG CARGO_PROFILE_RELEASE_LTO=thin
ARG CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
ENV ZPL_AGENT_GIT_COMMIT=${ZPL_AGENT_GIT_COMMIT}
ENV CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO}
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=${CARGO_PROFILE_RELEASE_CODEGEN_UNITS}
RUN apt-get update \
    && apt-get upgrade --yes \
    && apt-get install --yes --no-install-recommends libusb-1.0-0-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
RUN cargo build --locked --release

FROM scratch AS artifact
COPY --from=build /src/target/release/zpl-agent /zpl-agent

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
RUN apt-get update \
    && apt-get upgrade --yes \
    && apt-get install --yes --no-install-recommends libusb-1.0-0 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system zpl-agent \
    && useradd --system --gid zpl-agent --home-dir /var/lib/zpl-agent zpl-agent \
    && install -d -o zpl-agent -g zpl-agent /var/lib/zpl-agent /etc/zpl-agent
COPY --from=build /src/target/release/zpl-agent /usr/local/bin/zpl-agent
COPY config.example.toml /etc/zpl-agent/config.toml
USER zpl-agent
EXPOSE 8080/tcp 5353/udp
VOLUME ["/var/lib/zpl-agent"]
ENTRYPOINT ["/usr/local/bin/zpl-agent"]
CMD ["--config", "/etc/zpl-agent/config.toml"]
