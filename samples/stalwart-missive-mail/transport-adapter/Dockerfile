FROM docker.io/library/rust@sha256:3f6e6f8d8725a65a2db964bb828850f888d430c68784d661f753144e5d787207 AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM docker.io/library/debian@sha256:f3034a6ec3c1205360777c4aae76234998866ad18806ae62b63a3f84ccad782b

COPY --from=builder /src/target/release/ato-fixed-socks5-tls-adapter /usr/local/bin/ato-fixed-socks5-tls-adapter
USER 65532:65532
EXPOSE 2525
ENTRYPOINT ["/usr/local/bin/ato-fixed-socks5-tls-adapter"]

