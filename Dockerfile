FROM rust:1.91-slim-trixie AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

# Runtime stage: Distroless for security and small image
FROM gcr.io/distroless/cc
COPY --from=builder /app/target/release/line-bot /usr/local/bin/line-bot
EXPOSE 8080
ENTRYPOINT [ "/usr/local/bin/line-bot" ]
