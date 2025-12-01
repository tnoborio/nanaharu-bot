FROM rust:1.91-slim-trixie AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() { println!(\"placeholder\"); }" > src/main.rs && cargo fetch
RUN cargo build --release

RUN rm -rf src
COPY src ./src
RUN touch src/main.rs
RUN cargo build --release

# Runtime stage: Distroless for security and small image
FROM gcr.io/distroless/cc
COPY --from=builder /app/target/release/line-bot /usr/local/bin/line-bot
EXPOSE 8080
ENTRYPOINT [ "/usr/local/bin/line-bot" ]
