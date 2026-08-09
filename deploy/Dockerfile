# TriggerTrader paper stack: bot (executor+feed), crypto-portfolio (planner),
# futures-bot (the NQ noise book), api (dashboard and process supervisor). One
# image, four entrypoints - the same binaries a human runs, which is the §8.1
# rule that keeps cron and shell from drifting apart.
FROM oven/bun:1 AS frontend
WORKDIR /src/frontend
COPY frontend/package.json frontend/bun.lock ./
RUN bun install --frozen-lockfile
COPY frontend/ ./
RUN bun run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY service/ service/
RUN cd service && cargo build --release -p bot -p crypto-portfolio -p api -p futures-bot \
 && strip target/release/bot target/release/crypto-portfolio target/release/api \
          target/release/futures-bot

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates bash util-linux && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/service/target/release/bot /usr/local/bin/bot
COPY --from=build /src/service/target/release/crypto-portfolio /usr/local/bin/crypto-portfolio
COPY --from=build /src/service/target/release/api /usr/local/bin/api
COPY --from=build /src/service/target/release/futures-bot /usr/local/bin/futures-bot
COPY config/default.toml /app/config.toml
COPY deploy/config-patch.py /app/config-patch.py
COPY deploy/bot.cluster.json /app/bot.json
COPY deploy/cycle.sh /usr/local/bin/cycle.sh
COPY deploy/bootstrap-cycle.sh /usr/local/bin/bootstrap-cycle.sh
COPY deploy/futures-live.sh /usr/local/bin/futures-live.sh
COPY data/models/ranker-rank-1.json /app/ranker-rank-1.json
COPY --from=frontend /src/frontend/dist /app/frontend/dist
# the cluster config is the repo config with the model path pinned to the image
RUN python3 /app/config-patch.py || sed -i 's|^model_path = .*|model_path = "/app/ranker-rank-1.json"|' /app/config.toml
RUN useradd -u 1000 -m trader
USER 1000
WORKDIR /data
