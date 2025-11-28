# Builder stage
# Use a Rust toolchain new enough for reqwest/axum transitive ICU crates (needs 1.83)
FROM rust:1.83 as builder

WORKDIR /app

# 依存関係をキャッシュさせるために先に Cargo.toml をコピー
COPY Cargo.toml .

# ビルド用のダミー src を一旦置く
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true

# 本物のソースコードをコピー
COPY src ./src

RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/line-bot /usr/local/bin/line-bot

ENV RUST_LOG=info

# Cloud Run では PORT 環境変数が渡される
EXPOSE 8080

CMD ["line-bot"]
