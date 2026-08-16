# Crypto-Scalper — Project Status and Handoff

**As of:** 2026-08-16. **Written for:** a fresh agent (or human) picking this up
after a machine reinstall, with no memory of the sessions that built it.
Read this, then `docs/scalper-research.md` (the protocol), then the gate-run
records, in that order. Everything below is verifiable from git and the
committed docs; nothing here relies on anyone's recollection.

## 1. One-paragraph summary

We built, end to end, a research pipeline for a 1-minute-bar crypto perp
scalper (Rust everywhere except LightGBM fitting in Python), pointed it at
Binance USDT-M perps using Binance's free historical archives (klines,
bookDepth, aggTrades, funding, metrics), pre-registered a Sharpe > 2.0 gate,
and ran it four times. **Runs 1–3 are invalid** for signal purposes: a
look-ahead in the `metrics` join (5-minute rows stamped with the START of
their window; the join treated the stamp as the observation time) leaked five
minutes of the future into every fs-2/fs-3 model. **Run 4 (fs-4, the first
clean run) FAILS at all three horizons** — Sharpe 0.76 / 0.69 / 0.51 at
15/30/60 min, pooled IC 0.037 / 0.022 / 0.015. The protocol's FAIL branch is
invoked: no parameter tuning; any continuation is a *new pre-registered signal
program*. The user's last stated intent (2026-08-16) was to pause here, push,
back up, and reinstall the machine.

## 2. Where everything lives

| What | Where |
|---|---|
| Design spec | `docs/superpowers/specs/2026-08-12-crypto-scalper-design.md` |
| Implementation plans (executed) | `docs/superpowers/plans/2026-08-1{2,3,5}-crypto-scalper-plan-{1,2,3,3b,3c,3d}*.md` |
| **The protocol (binding)** | `docs/scalper-research.md` — §1–6 original + Amendments 1, 2, 3, 3b, 4 + "Research phase closed" |
| Fee snapshot | `docs/binance-um-fee-table-2026-08.md` (VIP0 verified; VIP1–8 unverified — needs the user's authenticated fetch) |
| Gate records | `docs/scalper-research-smoke.md` (plumbing only), `docs/scalper-gate-run-1.md`, `-2.md`, `-4.md` (run 3 was never written up separately; its numbers and the leak are in run 4's history table) |
| Rust: features | `service/crates/features-scalper` (`FEATURE_SET_VERSION = "fs-rust-scalper-4"`, 38 features; fs-1 golden values pinned in tests) |
| Rust: data + gate | `service/crates/scalper-data` — subcommands `pull-binance-perp`, `pull-binance-micro`, `record-books`, `summarize-costs`, `universe`, `training-matrix`, `binance-costs`, `gate` |
| Rust: venue | `service/crates/hyperliquid` (WS module + ALO/reduce-only/cancel-by-cloid — built for the original HL target; unused by the Binance research) |
| Python (fitting only) | `training/train_scalper.py`, `training/walk_forward_scalper.py`, `training/make_parity_fixture.py` (+ tests) |
| Ops scripts | `bin/scalper-{pull,record,backfill}.sh` |
| Frozen data (NOT in git) | `data/perp` (klines), `data/binance-micro/{book,flow,metrics,funding}` (6.9GB), `data/matrices/gate-run-{1,2,4}.jsonl`, `data/models/gate-run-*`, `data/reports/gate-run-*.json`, `data/binance-micro/costs-daily{,-3b}.json`, `data/scalper-universe.json` — backed up to `/mnt/tank/ai-trader-backup-2026-08-16/data/` |
| Agent memory | `~/.claude/projects/-home-magnus-dev-magnus-ai-trader/memory/` (backed up to `.../dot-claude/`) |

## 3. Hard rules the user set (do not violate)

1. **Python is training orchestration ONLY.** Features, inference, simulation,
   costs, Sharpe, gate verdict — all Rust. Python fits LightGBM and dumps JSON.
2. **Pre-registration before numbers.** Every parameter/definition/exit/cost
   change is written into `docs/scalper-research.md` as a dated amendment and
   committed BEFORE the code exists or the run happens. Post-hoc tuning is
   forbidden by the protocol's own text.
3. **Gate = annualized Sharpe strictly > 2.0 on daily net-of-cost returns,
   out-of-sample**, per-trade numbers never used; FAIL means stop or return to
   feature research — no threshold shopping.
4. Never touch `crypto-portfolio` (the frozen live strategy) or `data/bars`.
5. Never publish externally (no claude.ai artifacts/hosted pages).
6. Never write to `data/perp` from anything but the perp puller (a guard
   refuses a data-root containing the daily spot store).

## 4. What the four gate runs established

| run | features | costs | exits | window actually tradeable | h15 / h30 / h60 | validity |
|---|---|---|---|---|---|---|
| 1 | fs-2 | ±0.2%-band impact | time | ~6 mo (band starts 2026-01-15) | 6.31 PASS / 2.06 PASS / −2.96 FAIL — one fold each | not eligible; **contaminated (metrics leak)** |
| 2 | fs-3 | ±0.2%-band impact | time | ~6 mo (cost side still band-bound) | −1.78 / −1.09 / −1.00 all FAIL, 21 folds | eligible on the matrix span; **contaminated** |
| 3 | fs-3 | 3b (±1% band, m=2.3, floored) | ATR k=4 R:R 1.2 | full 24 mo | 17.9 / 16.9 / 15.0 all "PASS" | **the leak, exposed**: fold Sharpes 25–42 only in 2024–25; taker_ls_ratio 42% of gain |
| **4** | **fs-4** (causal metrics join) | 3b | ATR | full 24 mo | **0.76 / 0.69 / 0.51 all FAIL** | **clean; the record** |

Run-4 diagnostics (h15): 4,144 trades over 21 folds (14 traded, 7 positive),
net **+30 bps/trade after costs** but Sharpe 0.76; pooled IC 0.037 (real,
weak), rank IC ~0 by h60; entries rare because honest predictions rarely
clear 1.5× round-trip (~11–25 bps); ATR exits mechanically active at h60
(stops+targets > time exits) yet Sharpe lowest there. Read: a genuine but
order-of-magnitude-too-small signal; better exits cannot rescue an IC-0.02
entry.

## 5. Bugs found and fixed along the way (all committed, all tested)

- Metrics join look-ahead (Amendment 4, fs-4) — the big one.
- `oi_change_60` emitted `Some(NaN)` on zero-OI minutes → serialized as null
  → Rust gate refused the matrix. Fixed with a "Some only if finite"
  invariant across all features (`5f8e77a`).
- Three memory blowups on the 2.45M-row matrix (12GB WSL): Python dict loader
  → columnar (`55dcc76`); Rust gate loader → columnar (`48af1ca`);
  training-matrix builder still buffers all assets (~10GB) — run it with
  `ulimit -v 11500000`, everything else at 9000000. Always run fits/gates
  under a `ulimit -v` guard on this class of machine.
- Cost model bound to the ±0.2% band (Amendment 3b): pre-2026 days were all
  "thin → untradeable" until the ±1%-band model with the m=2.3 floor.
- Exit walk bar-count vs time-bound + gapped-stop fill at the open (`00278b6`).
- Recorder overran its hour under real fetch latency → wall-clock bound.
- Binance monthly archives lag a month → daily-archive fallback.
- k-prefix coins (`kPEPE` ↔ `1000PEPEUSDT` ↔ store key `KPEPE`): the universe
  file is the sole identity source; never call `binance_um_symbol` on a store key.

## 6. Open items and known gaps

- **Fee tiers VIP1–8 unverified** (fee page is JS-walled). If any future run
  projects volume above the VIP1 threshold, the fixed-point rule requires the
  user's authenticated fee-page fetch first.
- The `training-matrix` "book from 2026-01-15" coverage line is a stale
  diagnostic (it reports the ±0.2% fields' first date; fs-4 uses ±1%). Cosmetic.
- HyperLiquid: the original venue. Its WS module and order controls are done
  and tested; HL was parked in favor of Binance because Binance has free
  historical microstructure. `bin/scalper-record.sh` (HL book recording) was
  never cronned; no HL cost data exists beyond a few smoke samples.
- No live bot exists (plan 4 was gated on a PASS that never came). Nothing
  trades. Nothing is deployed.
- `docs/scalper-gate-run-4.md` may be the last commit on the branch — verify
  it exists; if not, the numbers above (from the reports on tank) are the
  record and the doc should be written from them.

## 7. If continuing (the FAIL branch)

The protocol allows a *new pre-registered signal program*, not tuning. The
three candidate directions noted (none endorsed, none tested):
(a) tick-level order-flow features from raw aggTrades (the archives are
already ingested per-minute; the raw trade tape is not stored);
(b) maker-side execution economics (post-only entries; round-trip ~4 bps vs
~11 — needs a fill model that is not optimistic, i.e. queue-position aware,
which the current simulator does not have);
(c) a different label/target than fixed-horizon return.
Any of these = new amendment → new feature-set version → matrix rebuild → fits
→ gate, exactly as runs 1–4 were done. Budget ~1–2 hours compute per full run
on a 12GB machine with the guards above.

## 8. How to restore on a new machine

1. `git clone git@github.com:MrTrigger/ai-trader.git` (all code + docs).
2. From `/mnt/tank/ai-trader-backup-2026-08-16/`: rsync `data/` and `var/`
   into the repo root; `repo-untracked/.env` → repo root; `dot-claude/` →
   `~/.claude/` (restores the agent's project memory).
3. `cd service && cargo build --release -p scalper-data`; `cd training && uv sync`.
4. `cargo test` (expect ~620+ passing) and `uv run pytest` in `training/`.
5. Sanity: `service/target/release/scalper-data gate --matrix
   data/matrices/gate-run-4.jsonl --folds data/models/gate-run-4-h15/folds.json
   --binance-costs data/binance-micro/costs-daily-3b.json --fee-taker-bps 4.5
   --fee-maker-bps 1.8 --notional 5000 --exit atr --data-root data --out
   /tmp/check.json` should reproduce `data/reports/gate-run-4-h15.json` byte
   for byte except `generated_utc`.
