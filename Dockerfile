# 构建阶段 - 分层缓存优化
FROM rust:bookworm AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# 依赖层：先克隆 pnos-spec + 复制 Cargo.toml，编译所有依赖（缓存友好，代码改动不失效）
RUN git clone https://github.com/PandaNetOS/pnos-spec.git ../pnos-spec
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

# 应用层：复制真实源码，只编译 pnos-runtime 本身
COPY . .
RUN touch src/main.rs && cargo build --release

# 产物阶段：只存二进制
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/pnos-runtime /usr/local/bin/pnos-runtime
