# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# 多阶段构建:前端 -> Rust 二进制 -> 极简运行镜像
# 前端产物在编译期被 rust-embed 嵌入二进制,最终镜像只有一个可执行文件。
# ---------------------------------------------------------------------------

# ---- 1. 构建管理界面(Vue SPA) -------------------------------------------
FROM node:20-bookworm-slim AS web
RUN corepack enable
WORKDIR /app/web
# 先只拷贝依赖清单,利用 Docker 层缓存
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ---- 2. 构建 Rust 二进制 --------------------------------------------------
FROM rust:1-bookworm AS builder
# Luau 需要 C 工具链(rust 镜像已带 gcc/make);cmake 供底层构建脚本使用
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
# 嵌入上一步构建好的前端
COPY --from=web /app/web/dist ./web/dist
RUN cargo build --release --locked

# ---- 3. 运行镜像 ----------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# rustls 校验上游 CA 需要根证书
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/tiny-station /usr/local/bin/tiny-station
# 配置 / 身份 / cluster key / 内核都放在 /data(用卷持久化)
WORKDIR /data
ENV RUST_LOG=tiny_station=info
ENTRYPOINT ["tiny-station", "-c", "/data/tiny-station.toml"]
