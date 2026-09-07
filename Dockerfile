# One static binary, so the runtime image carries nothing but the binary and a
# CA bundle for outbound webhooks.
FROM rust:1.85-alpine AS build
RUN apk add --no-cache musl-dev git

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release --bin spacetrace-hub

FROM alpine:3.21
RUN apk add --no-cache ca-certificates wget \
    && adduser -S -H -u 10002 spacetrace

COPY --from=build /src/target/release/spacetrace-hub /usr/local/bin/

RUN mkdir -p /var/lib/spacetrace-hub && chown spacetrace /var/lib/spacetrace-hub
VOLUME /var/lib/spacetrace-hub

USER spacetrace
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD wget -qO- http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/spacetrace-hub"]
CMD ["--config", "/etc/spacetrace-hub/hub.toml", "serve"]
