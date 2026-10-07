FROM node:22-bookworm-slim AS web
WORKDIR /web
# corepack 启用 pnpm；锁文件与源码同目录
COPY web/package.json web/pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM rust:bookworm AS rust
WORKDIR /src
COPY . .
RUN cargo build --release -p api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg && rm -rf /var/lib/apt/lists/*
COPY --from=rust /src/target/release/crawler-media /usr/local/bin/crawler-media
COPY --from=web /web/dist /usr/share/crawler-media/ui
ENV CRAWLER_MEDIA_DATA=/data
ENV CRAWLER_MEDIA_LISTEN=0.0.0.0:18765
ENV CRAWLER_MEDIA_UI=/usr/share/crawler-media/ui
EXPOSE 18765
VOLUME ["/data"]
CMD ["crawler-media"]
