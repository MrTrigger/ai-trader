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
COPY deploy/bot.cluster.json /app/bot.json
COPY deploy/cycle.sh /usr/local/bin/cycle.sh
COPY deploy/bootstrap-cycle.sh /usr/local/bin/bootstrap-cycle.sh
COPY deploy/futures-live.sh /usr/local/bin/futures-live.sh
COPY deploy/ib-gateway-on-demand.sh /usr/local/bin/ib-gateway-on-demand.sh
COPY data/models/ranker-rank-1.json /app/ranker-rank-1.json
COPY --from=frontend /src/frontend/dist /app/frontend/dist
# The cluster config is the repo config with the model path pinned to the
# image. sed, not an interpreter: there is no Python in the runtime by rule,
# and the previous `python3 ... || sed ...` quietly fell through to the sed on
# every build while pretending the Python mattered.
RUN sed -i 's|^model_path = .*|model_path = "/app/ranker-rank-1.json"|' /app/config.toml \
 && grep -q '^model_path = "/app/ranker-rank-1.json"$' /app/config.toml \
 && chmod +x /usr/local/bin/ib-gateway-on-demand.sh
# No interpreter in the runtime, by rule: Python belongs to training, and the
# deployed image runs compiled binaries and POSIX sh. Asserted at build time
# rather than trusted, because the last violation was a bootstrap script that
# shelled out to python3, swallowed the "command not found", and read the
# silence as "this bot has never run" - which restarted a live book's cycle on
# every pod restart.
RUN for i in python python3 ruby node; do \
      if command -v "$i" >/dev/null 2>&1; then \
        echo "runtime image must not contain $i - see deploy/Dockerfile" >&2; exit 1; \
      fi; \
    done

RUN useradd -u 1000 -m trader
USER 1000
WORKDIR /data
