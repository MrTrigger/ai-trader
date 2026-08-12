# Operations

Two places this system runs, and they are not the same thing.

- **Laptop** — research, backtests, and a paper book you can throw away. Loopback
  only, state under `var/`, nothing scheduled.
- **Cluster** (`triggerlab`, namespace `trader`) — the Phase 2 paper run. Runs
  unattended, keeps its state on a PVC and its records in Postgres, and is the
  only instance whose numbers count.

Everything below is the whole loop: change code, ship it, watch it, intervene.

---

Two bots, and they are driven differently on purpose:

| | `crypto-portfolio` | `futures-noise` |
|---|---|---|
| decides | once a day, on the daily bar | every 5-minute bar close |
| driven by | a CronJob running `cycle.sh` | a resident process, supervised by the api |
| venue | Hyperliquid (paper book) | IB Gateway (paper account, armed) |
| controls | CLI, audited by kubeconfig | the dashboard's Start/Halt/Stop |

---

## 1. The daily cycle, and why it is a script

This section is the **crypto** bot. The futures bot has no cycle — it is a loop
that never stops between bars, §3.

One decision-and-execution cycle is four commands, in `deploy/cycle.sh` on the
cluster and `bin/cycle.sh` on the laptop:

```
data-pull        refresh daily and hourly bars
universe-rank    record today's point-in-time universe, screened to what the venue lists
plan             decide, against the venue's book, with the frozen model
bot run          execute the plan across its slices
```

Design spec §8.1: **the scheduler invokes the same commands a human does.** No
private path into the engine, so cron and your shell cannot drift apart. Every
stage failing is fatal to the cycle — a plan built on stale data is worse than
no plan.

The cycle takes 5–10 minutes, dominated by `data-pull` walking ~670 assets (most
long delisted, and kept deliberately: dropping them would reintroduce
survivorship bias into the universe ranking).

---

## 2. Laptop: research and a local paper book

```bash
cd ~/dev/magnus/ai-trader
cargo build --release -p crypto-portfolio -p bot -p api   # in service/

# a full cycle against the local paper book
bin/cycle.sh var/live/bot.json

# the dashboard (loopback, no flag to change that). It supervises: any bot whose
# control says running and whose `launch` is recorded is kept alive. Add
# --no-supervise when you want to run one from your own terminal instead.
./service/target/release/api --state-dir var/live/state \
  --initial-cash 100000 --quote-currency USDC \
  --bot ./service/target/release/bot --bot-config var/live/bot.json
```

The futures bot on the laptop is `bots/futures/run.sh` — `live` to arm, `shadow`
for simulated fills, `parity` for the gate. Its warmup bars come from the
journal's parquet store through Python; in the image there is no Python, so the
Gateway's own history is the source. Same binary, same strategy, one plumbing
difference. `bots/futures/README.md` has the rest.

### Research: walk-forward is the only measurement that counts

```bash
# 1. the matrix (Rust owns every feature; ~25 min)
service/target/release/crypto-portfolio training-matrix \
  --config config/default.toml --data-root data \
  --start 2019-10-01 --end 2026-08-01 --out data/models/training.jsonl

# 2. six expanding folds, a model retrained at each, two-day purge (~15 min)
bin/walk-forward.sh 2022-09-18 2026-07-30 6 var/research/wf-experiment per_risk

# 3. the report: equity, drawdown, leverage panels, vs BTC and the S&P
python3 bin/build-research-report.py var/research/wf-experiment \
  <spx.json> <btc.json> docs/research/report.html
```

Knobs, all environment variables read by `training/train.py`, all reproducible:

| variable | meaning |
|---|---|
| `TRAIN_RANK=1` | fit the within-date rank of the reward (**the shipped setting**) |
| `TRAIN_SEEDS=n` | average n boosters (concatenated trees, leaves scaled 1/n) |
| `TRAIN_DROP=a,b` | exclude features from the fit |
| `TRAIN_OBJECTIVE=` | `l2` (default), `l1`, `huber` |

and on `training-matrix`: `--hold-hours`, `--step-hours`, `--include-unlisted-training`,
`--leaky-funding-diagnostic` (see §7).

**The signal is frozen.** Every additional configuration measured against
2022–2026 degrades the Sharpe estimate that justifies trading it. Reopen only on
live evidence. `docs/phase-1-findings.md` records what was tried and rejected.

---

## 3. Cluster: what is deployed

Both bots live in one pod, against the cluster's Postgres. `/data` is a cache;
nothing durable is file-resident.

| piece | what it is |
|---|---|
| `CronJob aitrader-paper-cycle` | the crypto decision cycle, 00:05 UTC daily |
| `Deployment aitrader-paper` | on-demand `ib-gateway` controller (sidecar), `feed`, `api` — and `futures-bot` as a child of `api` |
| `PVC aitrader-paper-data` | bars, universe snapshots, funding, paper venue state, the futures bar cache, IB Gateway settings (10Gi, `local-path`, pinned to `talos-2`) |
| `Secret aitrader-paper-secret` | `DATABASE_URL`, `TWS_USERID`, `TWS_PASSWORD`, `IB_PAPER_ACCOUNT` — sops-encrypted in git |
| Postgres `ai_trader` | identity registry, controls, runs, fills, ledger, paper book |

- Dashboard: **https://trader.wallintech.eu** (also a homepage tile)
- The manifests live in `triggerlab`, at
  `kubernetes/apps/trader/aitrader-paper/`

### The futures bot is a process, not a container

`futures-noise` runs as a child of the `api` container, launched by the command
recorded in `bots.launch` (`/usr/local/bin/futures-live.sh`). That is not a
packaging convenience, it is what makes the control states mean anything:

- **Stop ENDS the process.** A container that exits is a container the kubelet
  restarts, so a stopped bot in its own container would come straight back —
  crash-looping on the operator's own instruction. With the api as its parent,
  stopped means stopped and burns no CPU.
- **Start launches one**, and the api **supervises**: every 30s it checks each
  bot whose control says `running`, and relaunches any that is not publishing
  (90s without a heartbeat, or a final document saying it exited). A SIGKILLed
  bot is back inside two minutes without anybody watching.
- A control word with no row reads as **halted**, never running. Registration
  enables a bot; it never starts one.

`--no-supervise` turns that off, for when you are driving a bot from your own
terminal and do not want a second one appearing underneath you.

### IB Gateway runs on demand in the pod

The futures bot needs a Gateway and a Gateway needs a desktop, so the pod has a
**native sidecar** — an init container with `restartPolicy: Always`. The
sidecar's resident process is only a small demand controller. The expensive and
exclusive part (`ghcr.io/gnzsnz/ib-gateway`: IBC + Gateway + Xvfb + socat) is
started only while at least one supervised bot process whose trade binding uses
the `ib` protocol holds a lease. The last process releasing its lease shuts the
Gateway down after 45 seconds. `Halt` retains the lease because the process is
still managing its book; `Stop` releases it only after the process has flattened
and exited. Separate lease files make the rule work unchanged when more than one
IB-backed bot exists.

### The lease says which money, because the login depends on it

Paper and live are not a flag on one IB session — they are **different IBKR
usernames holding different accounts**. So the lease carries the leasing bot's
bound `account_kind`, and the controller exports the matching credential pair
(`IB_PAPER_USERNAME`/`IB_PAPER_PASSWORD` or the `IB_LIVE_*` pair) plus
`TRADING_MODE` before starting the session. Change a bot's binding and the next
launch brings up a Gateway logged into the other account; if one is already up
for the other kind, the controller stops it and switches.

Both credential pairs are in the sops secret, and that is deliberately not the
same thing as permission to use them:

| gate | what it stops |
|---|---|
| `IB_ALLOW_LIVE_GATEWAY` (sidecar, `no`) | opening a real-money **broker session** at all |
| `IB_ALLOW_LIVE` (bot, `no`) | placing orders on a live account |
| the trade binding (`ib-paper`) | which account the bot even asks for |

Binding a bot to the live account is one click on the dashboard's venue picker.
Opening a live session is a manifest change, reviewed in git. The controller
refuses a live lease while the gate is shut and says so, rather than starting a
paper Gateway that the live-bound bot would then refuse ten minutes later —
which looks exactly like a bad password.

A Gateway holding the wrong account is not a soft failure either way: the bot
asks the Gateway which accounts it holds and refuses if its own is not among
them.

The API also owns the launcher's process group. If `Stop` arrives while an IB
readiness/backfill script is still waiting and no heartbeat from that launch
has ever been published, the API terminates that group and its reaper releases
the lease. Once a launch has published, it is never killed by this shortcut:
the bot must consume `stopped`, flatten, publish its terminal state, and exit.
This distinction prevents both a pre-loop launcher from pinning Gateway open
and a stale heartbeat from turning Stop into an unsafe hard kill.

Containers in a pod share a network namespace, so an active bot reaches the
Gateway at `127.0.0.1:4004` (socat's relay of the Gateway's loopback-only 4002).
Nothing outside the pod can: the NetworkPolicy admits 7434 and nothing else,
and the IB API is an unauthenticated raw socket. With no IB-backed bot active,
there is no Java process, API listener, or IB login in the cluster.

**One session per IBKR username.** IB allows a single Gateway login per user, and
`EXISTING_SESSION_DETECTED_ACTION=primary` means an active cluster Gateway takes
it. Starting an IB-backed cluster bot can therefore **kick out the Gateway on
your laptop**, and the bot attached to it goes blind. Stopping the last
IB-backed cluster bot releases that session automatically; running both still
requires a second paper user.

While demanded, the Gateway restarts itself at 21:15 UTC
(`AUTO_RESTART_TIME`), because IB expires the session daily. That is inside
CME's 16:00–17:00 CT maintenance break, so there are no bars to miss — and the
bot's stall watchdog does not count silence while the market is closed. The
demand controller has **no Gateway readiness probe on purpose**: pod readiness
gates the Service, and a dashboard that vanishes exactly when the broker link
does could not report the one thing you would want to know.

### The warmup bars come from IB, not from the lab

The noise band needs 14 completed sessions before it reads at all, and the
laptop fills that from the journal's parquet store through Python — which is not
in the image. So `futures-live.sh` waits for the Gateway and then runs
`futures-bot backfill --days 45`, which **seeds** when there is no cache and
extends when there is. A cache whose last bar predates the window is replaced
rather than appended to: a noise band computed across a hole is a number with no
meaning.

### What the dashboard can do, and to which bot

The crypto half is still a **lens**: that api runs without `--bot`, so its
single-bot controls refuse, and its cycle belongs to the CronJob. Interventions
go through `kubectl exec` (§6), authenticated by your kubeconfig.

The futures half is a **hand**. Start, Halt and Stop on the bot page write
`control_events` and reach a real process — Halt is a graceful stop (no new
entries, open positions still close when the strategy says so), Stop exits the
process. The NetworkPolicy, restricting ingress to the gateway and the homepage
monitor, is what decides who can press them.

### Which credentials are in this stack, and which are not

There is no `HL_AGENT_PRIVATE_KEY` in the image, the manifests, or the cluster:
crypto paper needs public market data and an address, and going live is a
deliberate, separate act.

The futures side does hold credentials, because a broker session cannot be
opened without them: the IBKR **paper** username and password, in sops. Order
flow is armed against the paper account (`IB_ALLOW_ORDERS=yes`,
`IB_ALLOW_LIVE=no`, bound to `ib-paper`); reaching live capital would take both
a flag flip and a binding change. `FUTURES_LIVE=no` drops the bot to shadow —
simulated fills, published, nothing sent — as a one-line change.

---

## 4. Ship a change

The image is built by GitHub Actions on every push touching `service/`,
`config/default.toml` or `deploy/`, tagged both `:latest` and `:<commit-sha>`.

**The cluster only ever runs a SHA tag.** Never `:latest` — spegel mirrors the
registry and serves from node cache, so a mutable tag is whatever that node
last happened to see. A `rollout restart` against `:latest` can silently
re-run the old image while reporting success, which cost two debugging rounds
of arguing with a pod that was confidently running week-old code. An immutable
tag also makes the deployed version a fact in git rather than a question.

```bash
# 1. change code, and prove it
cd service && cargo test --release && cargo fmt --all

# 2. push, and wait for THAT COMMIT to build (~5 min)
git push
SHA=$(git rev-parse HEAD)
gh run list -R MrTrigger/ai-trader --workflow paper-image --limit 5 \
  --json headSha,status,conclusion -q ".[] | select(.headSha==\"$SHA\")"
```

Watch for the right SHA, not the newest run: `--limit 1` returns whatever
finished last, which right after a push is still the *previous* build. Waiting
on that exits immediately and deploys the old image.

```bash
# 3. pin the cluster to it (in the triggerlab repo)
cd ~/dev/magnus/triggerlab
sed -i "s|aitrader-paper:.*|aitrader-paper:$SHA|" \
  kubernetes/apps/trader/aitrader-paper/app/{deployment,cronjob}.yaml
git commit -am "aitrader-paper: $SHA" && git push

# 4. let Flux take it (or force it)
export KUBECONFIG=~/dev/magnus/triggerlab/kubeconfig
kubectl annotate gitrepository flux-system -n flux-system \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite
kubectl annotate kustomization aitrader-paper -n trader \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite
kubectl rollout status deployment/aitrader-paper -n trader
```

Confirm what is actually running rather than trusting the rollout:

```bash
kubectl get deploy -n trader aitrader-paper \
  -o jsonpath='{.spec.template.spec.containers[1].image}'
kubectl logs -n trader deploy/aitrader-paper -c api --tail=5   # the startup banner
```

CronJobs pick up the pinned image on their next run; nothing to restart.

**Manifest changes** live in the `triggerlab` repo and go through Flux:

```bash
cd ~/dev/magnus/triggerlab
kubectl kustomize kubernetes/apps/trader/aitrader-paper/app   # validate first
git commit && git push
kubectl annotate gitrepository flux-system -n flux-system \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite
kubectl annotate kustomization aitrader-paper -n trader \
  reconcile.fluxcd.io/requestedAt="$(date +%s)" --overwrite
```

### Shipping a new model

The artefact is committed (`data/models/ranker-rank-1.json`, 2.8MB) and baked
into the image, because it *is* the deployable strategy — leaving it out of git
would leave the strategy behind.

```bash
TRAIN_RANK=1 training/.venv/bin/python training/train.py \
  --matrix data/models/training.jsonl --through <cutoff> \
  --reward per_risk --out data/models/ranker-rank-1.json
git add -f data/models/ranker-rank-1.json && git commit && git push   # CI + rollout
```

Rust refuses an artefact whose feature-catalogue version or ordered feature
names disagree with its own, and refuses to score any date at or before
`trained_through`. Both are hard failures, not warnings.

---

## 5. Watch it

```bash
export KUBECONFIG=~/dev/magnus/triggerlab/kubeconfig
kubectl get pods,cronjob,job -n trader | grep aitrader
kubectl logs -n trader deploy/aitrader-paper -c feed --tail=20        # marks feed
kubectl logs -n trader deploy/aitrader-paper -c api  --tail=20        # dashboard + supervisor
kubectl logs -n trader deploy/aitrader-paper -c ib-gateway --tail=40  # IBC login, restarts
kubectl logs -n trader job/<the-cycle-job> | tail -40                 # a cycle

POD=$(kubectl get pod -n trader -l app.kubernetes.io/name=aitrader-paper \
        --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}')
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json status
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json positions
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json reconcile
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json history --limit 10
```

Run a cycle by hand (the CronJob's own definition, so it cannot drift):

```bash
kubectl create job -n trader cycle-manual --from=cronjob/aitrader-paper-cycle
```

### Watching the futures bot

```bash
# is there a process, and whose child is it
kubectl exec -n trader $POD -c api -- ps -o pid,ppid,etime,cmd -C futures-bot

# what it printed: the wait for the Gateway, the backfill, the loop
kubectl exec -n trader $POD -c api -- tail -40 /data/var/futures-noise.launch.log

# is the Gateway answering at all
kubectl exec -n trader $POD -c api -- futures-bot ib-check
```

`ib-check` connects on client id 8 and the loop uses 9, so the probe is safe
while the bot runs. `backfill` uses 7 for the same reason: IB refuses a duplicate
id and can hold a just-released one reserved, so two phases of one deployment
sharing an id fails intermittently and only under load.

### What Phase 2 is actually grading

**Plumbing and cost realization, not P&L.** Three months of paper says almost
nothing about a Sharpe — confirming a true Sharpe of 2 at two sigma takes about
eleven months. What it settles in weeks: realized slippage against the 0.5bp
assumption, fill behaviour, funding actually paid, universe churn, stale symbols,
API failures, partial fills, and reconciliation.

Written down before the run started, so they cannot be rationalised later:

- Expected live Sharpe **1.0–1.3** (backtest 2.0–2.2)
- Kill: drawdown **> 25%**, realized costs **> 1.5×** modelled, or rolling 60-day
  IC **< 0**

---

## 6. Intervene

All four controls write an audited row to `control_events` and take a name and a
reason:

```bash
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json \
  halt   --reason "..." --by magnus     # stop executing; planning continues
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json \
  pause  --reason "..." --by magnus     # risk-reducing orders only
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json \
  resume --reason "..." --by magnus
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json \
  flatten --reason "..." --by magnus --confirm    # cancel resting, close at market
```

`halt` and `flatten` are different tools: halt stops new execution and leaves the
book alone; flatten closes it and works even while halted.

Suspend the schedule entirely:

```bash
kubectl patch cronjob aitrader-paper-cycle -n trader -p '{"spec":{"suspend":true}}'
```

### When the futures bot halts ITSELF

Kill criterion, reconcile mismatch, refused order, feed stall: these are **rail
halts**, and they latch. They live in the snapshot, so they survive restarts —
`Start` clears an operator halt and deliberately does not clear these. The pill
shows `halted · <reason>` in red, and it outranks whatever the feed is saying.

Investigate first, then clear it with the broker's agreement:

```bash
kubectl exec -n trader $POD -c api -- futures-bot ib-check      # what does IB hold?
# stop the bot first: a live process republishes the halt from its own snapshot
curl -X POST localhost:7434/api/bots/futures-noise/stop
kubectl exec -n trader $POD -c api -- futures-bot clear-halt \
  --by magnus --reason "IB flat, mismatch was a propagation race"
```

`clear-halt` reads the broker and the stored book and **refuses unless they
agree** — it cannot be used to paper over a real divergence, which is the whole
reason the latch exists. Then press Start.

### Intervening on the futures bot

Use the dashboard: **Start**, **Halt**, **Stop** on the bot page. They write the
same audited `control_events` rows, and unlike the crypto controls they reach a
process that acts within a second (Postgres `NOTIFY`, not a poll).

| you press | what happens |
|---|---|
| **Halt** | graceful stop: no new entries, open positions still close when the strategy exits them |
| **Stop** | flat and gone: the book closes, the process exits, and the supervisor leaves it alone |
| **Start** | a fresh process, publishing in ~7s |

If the ingress is what is broken rather than the bot, go straight at the api —
same endpoints, authenticated by your kubeconfig:

```bash
kubectl port-forward -n trader $POD 7434:7434 &
curl -X POST localhost:7434/api/bots/futures-noise/halt   # or /stop, /resume
```

### The gates that will stop a run, and what each means

| refusal | meaning |
|---|---|
| `bot "X" is registered but disabled` | fail-closed identity registry; `identity enable` deliberately |
| `plan ... is N minutes old` | the plan sat too long — build a new one |
| `decision lag ... past the limit` | the decision is far behind the fill; paper can override with `--accept-decision-lag`, live cannot |
| `is in Dry mode, not live` | the plan was not stamped `--for-execution` |
| `the venue reports a fill we never authorised` | reconciliation failure — **investigate before doing anything**; §7 |
| `model trained through D cannot score D` | leakage guard. Never relax it |

---

## 7. First principles worth not relearning

**Reconciliation disagreements are never auto-corrected.** The ledger is an
independent record of what we authorised, written before orders go out. If the
venue reports a fill the ledger does not know, the run stops. It could be another
process, a stale order, a compromised key — or a lost ledger. Those look
identical from inside. Check the account, then `bot adopt --accept-unknown-fills`
records *your judgement* against *your name*.

**The leakage guard is the reason any of these numbers can be believed.** A
recorded +875% (Sharpe 2.70) turned out to be a one-character bug: the funding
features summed *forward* from each decision date over realised rates. Flip the
window to trailing, change nothing else, and the same code returns +134.9% at
Sharpe 1.05. That leak is reproducible on purpose behind
`--leaky-funding-diagnostic`, which prints a warning that everything downstream
of it is fiction — it exists so the Rust port could be proven equivalent to the
Python harness, and for no other reason.
`docs/research/harness/README.md` has the full account.

**Bootstrapping a fresh database** (only ever needed once per environment):

```bash
kubectl exec -n storage postgres-0 -- createdb -U postgres ai_trader
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json identity migrate
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json identity register
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json identity enable
kubectl exec -n trader $POD -c feed -- bot --config /app/bot.json \
  resume --reason "..." --by magnus
```

The futures bot needs none of that. Its identity is written on every pod start
by the `register-futures` init container — account, trade binding, `state_dir`,
`launch`, enabled, idempotent — which migrates the schema first, so a fresh
database is deployable with nothing run by hand. It is registered and enabled
and **not started**: the control word decides that, and no control row reads as
halted. Press Start once.

A new PVC needs the *crypto* bar store seeded, or the first cycle spends hours
rebuilding history it could have copied:

```bash
tar czf /tmp/seed.tgz -C data bars funding universe
kubectl cp /tmp/seed.tgz trader/$POD:/data/seed.tgz -c feed
kubectl exec -n trader $POD -c feed -- sh -c 'cd /data && tar xzf seed.tgz && rm seed.tgz'
```

The futures bar cache is not seeded by hand — `futures-live.sh` fetches 45 days
from the Gateway on first launch.

**A fresh IB paper account** needs `IB_PAPER_ACCOUNT` in the sops secret, plus
the Gateway's own login:

```bash
cd ~/dev/magnus/triggerlab
sops kubernetes/apps/trader/aitrader-paper/app/secret.sops.yaml   # TWS_USERID, TWS_PASSWORD
```

Both start as `REPLACE_ME`, and IBC fails the login loudly rather than quietly
running without a broker. Which account the Gateway happens to hold is never
inferred: a mismatch with `IB_PAPER_ACCOUNT` is a refusal, because that is how a
paper bot meets a live account.

---

## 8. Environment gotchas, paid for once

- **`docker build` has no DNS here** — use `--network=host`.
- **The in-cluster registry cannot serve fresh pushes.** Spegel owns the nodes'
  wildcard registry config and resolves from its cache, so containerd answers
  `not found` for an image the registry is serving happily over its NodePort.
  Build in CI to ghcr instead.
- **`${SECRET_POSTGRES_PASSWORD}` does not exist** in `cluster-secrets`, despite
  appearing in other apps' manifests (unsubstituted there too). Use a sops
  secret, and add `decryption:` to the app's `ks.yaml` — child Kustomizations do
  not inherit it.
- **Docker's default OCI image index is unpullable by this containerd.** Build
  with `--provenance=false` if you ever push locally.
- **`:latest` is a lie on these nodes.** Spegel serves it from cache, so a
  rollout can succeed while running old code. Pin the SHA; verify the running
  image and the startup banner afterwards.
- **`gh run list --limit 1` right after a push** returns the previous run, not
  yours. Select on the commit SHA or you will deploy the thing you just
  replaced.
- **The api binds loopback** unless `KUBERNETES_SERVICE_HOST` is present, in
  which case it binds wide. Detected, never configured — a flag is a thing that
  gets set.
- **`containers[1]` was the api** until the pod grew a Gateway. `bin/deploy.sh`
  now selects it by name; an index that quietly points at a different container
  verifies the wrong image and reports success.
- **`pgrep -f futures-bot` matches its own shell.** The command line containing
  the pattern is itself a process, so a check for "is the bot running" answers
  yes when nothing is. `pgrep -x futures-bot` asks about the executable.
- **IB allows one Gateway session per username.** Bringing up the cluster
  Gateway logs the laptop's out from under whatever is attached to it.
- **A sidecar with a readiness probe gates the Service.** The Gateway restarts
  nightly; probing it would take the dashboard down with it, exactly when you
  would want to look at the dashboard.
