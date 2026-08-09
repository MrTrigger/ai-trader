# Operations

Two places this system runs, and they are not the same thing.

- **Laptop** — research, backtests, and a paper book you can throw away. Loopback
  only, state under `var/`, nothing scheduled.
- **Cluster** (`triggerlab`, namespace `trader`) — the Phase 2 paper run. Runs
  unattended, keeps its state on a PVC and its records in Postgres, and is the
  only instance whose numbers count.

Everything below is the whole loop: change code, ship it, watch it, intervene.

---

## 1. The daily cycle, and why it is a script

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

# the dashboard (loopback, no flag to change that)
./service/target/release/api --state-dir var/live/state \
  --initial-cash 100000 --quote-currency USDC \
  --bot ./service/target/release/bot --bot-config var/live/bot.json
```

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

| piece | what it is |
|---|---|
| `CronJob aitrader-paper-cycle` | the decision cycle, 00:05 UTC daily |
| `Deployment aitrader-paper` | two sidecars: the Hyperliquid marks feed, and the dashboard |
| `PVC aitrader-paper-data` | bars, universe snapshots, funding, paper venue state (10Gi, `local-path`, pinned to `talos-2`) |
| `Secret aitrader-paper-secret` | `DATABASE_URL` only — sops-encrypted in git |
| Postgres `ai_trader` | identity registry, controls, runs, fills, ledger, paper book |

- Dashboard: **https://trader.wallintech.eu** (also a homepage tile)
- The manifests live in `triggerlab`, at
  `kubernetes/apps/trader/aitrader-paper/`

### The dashboard on the cluster is a lens, not a hand

It runs **without `--bot`**, so its control endpoints refuse. Interventions go
through `kubectl exec` (§6), where they are authenticated by your kubeconfig and
audited by the cluster. Ingress is restricted by NetworkPolicy to the gateway and
the homepage monitor.

The laptop instance keeps its hands — it is loopback-only and drives a throwaway
book.

### No venue credentials anywhere in this stack

Paper needs public market data and the account address. There is no
`HL_AGENT_PRIVATE_KEY` in the image, the manifests, or the cluster. Going live
is a deliberate, separate act.

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
kubectl logs -n trader deploy/aitrader-paper -c feed --tail=20     # marks feed
kubectl logs -n trader deploy/aitrader-paper -c api  --tail=20     # dashboard
kubectl logs -n trader job/<the-cycle-job> | tail -40              # a cycle

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

A new PVC also needs the bar store seeded, or the first cycle spends hours
rebuilding history it could have copied:

```bash
tar czf /tmp/seed.tgz -C data bars funding universe
kubectl cp /tmp/seed.tgz trader/$POD:/data/seed.tgz -c feed
kubectl exec -n trader $POD -c feed -- sh -c 'cd /data && tar xzf seed.tgz && rm seed.tgz'
```

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
