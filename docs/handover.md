# Handover — moving development into WSL

> **Point-in-time, not a design document.** Written 2026-08-03 at commit `42451e3`, for an agent
> picking this up inside WSL. Everything durable lives in
> [`design-spec.md`](./design-spec.md) — read §0 there before writing code. **Delete this file
> once the move is done and the Rust crate has compiled at least once.**

---

## 1. The single most important thing

**The Rust crate has never been compiled.** `service/crates/plan/` — roughly 250 lines of types,
parsing and invariant checks — was written on a Windows machine with no MSVC linker, so `cargo
build` has never run against it. It may not compile. It is committed anyway because CI would have
caught it, but CI has also never run.

Treat every claim about the Rust half as unverified. Everything on the Python side *is* verified:
85 tests, passing.

**First job: `cd service && cargo test`.** Expect to fix compile errors. That is not a surprise,
it is the known state.

---

## 2. Why the move

Production is a Debian pod in Kubernetes. Development was on Windows, which caused two problems in
one session:

1. No MSVC linker, so the Rust half could not be built at all.
2. A real defect: `Path.write_text` translated `\n` to `\r\n`, so the Plan fixture — whose *bytes
   are the contract* between the Python planner and the Rust executor — was committed with CRLF.
   CI would have failed on its first run. Fixed in `42451e3`; the lesson is in §7 below.

WSL gives the right target triple (`x86_64-unknown-linux-gnu`), gcc, and the same linker CI uses.
Docker is also there, which is where the production image gets built.

---

## 3. Environment setup

The repo is at `/mnt/c/dev/magnus/ai-trader`. Nothing in it is platform-specific; only the local
environment needs rebuilding.

```bash
cd /mnt/c/dev/magnus/ai-trader

# Python. The existing planner/.venv is a Windows venv - it will not work here.
# It is gitignored, so just remove and recreate.
rm -rf planner/.venv
python3 -m venv planner/.venv
planner/.venv/bin/pip install -e "planner[dev]"
cd planner && pytest -q          # expect 85 passed
cd ..

# Rust. rustup was being installed when this was written; verify it landed.
source $HOME/.cargo/env
rustc --version
cd service && cargo test         # THE FIRST REAL TASK - see section 1
```

**Market data is gitignored** and must be re-pulled (about a minute, public API, no account):

```bash
planner/.venv/bin/ai-trader data pull --days 400
planner/.venv/bin/ai-trader universe record
planner/.venv/bin/ai-trader book init --cash 100000
planner/.venv/bin/ai-trader plan --as-of 2026-08-01
planner/.venv/bin/ai-trader plan verify --runs 3     # the Phase 0 gate
```

### Environment gotchas

- **WSL runs as root.** `$HOME` is `/root`, so the toolchain and venv land in root's home. Works,
  but flagged to the user as something to change now rather than later. **Do not change it
  yourself** — see §6.
- **Rust builds on `/mnt/c` are slow** (9p mount). If painful, set `CARGO_TARGET_DIR` to somewhere
  on the WSL filesystem so source stays on `/mnt/c` and artifacts go on ext4. Not yet decided.
- **Two rustup installs raced** during setup and corrupted the toolchain directory. If `rustc`
  reports `Missing manifest`, clear `~/.rustup/toolchains/*` and `~/.rustup/downloads/*` and
  reinstall once.

---

## 4. Where things stand

Phase 0 per [design spec §9](./design-spec.md#9-phased-build-order). No venue account, no capital,
and no strategy — the Phase 0 signal is an explicit placeholder that claims no edge and says so on
every plan it emits.

| Piece | State |
|---|---|
| Plan schema (`schema/plan.schema.json`) | done, v1.1.0 |
| Python planner: bars, store, source, universe, decision path, CLI | done, 85 tests |
| Determinism gate (`plan verify`) | **passing** — same decision twice is one plan |
| Rust `plan` crate + round-trip test | written, **never compiled** |
| CI (`.github/workflows/ci.yaml`) | written, **never run** |
| `paper` venue adapter | **not started** — last Phase 0 piece, belongs in Rust |

### Phase 0's exit gate, and how much of it holds

> `plan --dry-run` produces byte-identical plans across two runs, **and** Rust parses a
> Python-written Plan in CI.

First half: verified locally and covered by `planner/tests/test_pipeline.py`, which runs the gate
on synthetic bars so CI enforces it without shipping a data set. Second half: not verified at all.

---

## 5. What to do, in order

1. **Compile the Rust crate.** Fix whatever breaks. The types must keep matching
   `schema/plan.schema.json` — if you change a Rust type, check the schema, not just the test.
2. **Run the cross-language check by hand** before trusting CI:
   ```bash
   cd planner && AI_TRADER_UPDATE_FIXTURE=1 pytest tests/test_fixture.py -q
   git diff --exit-code -- service/crates/plan/tests/fixtures/plan.json   # must be clean
   cd ../service && cargo test -p plan --test roundtrip
   ```
   A clean `git diff` here is the whole point: it proves the fixture Python produces on Linux is
   byte-identical to the one committed from Windows. If it is not, the §7 lesson was not fully
   applied and that is more important than whatever else you were doing.
3. **Get CI green.** It has never run. Expect small things.
4. **Then, and only then, the `paper` venue adapter** — the last Phase 0 piece.

Do not start Phase 1 (a real strategy). Phase 0's gate is not met until step 3.

---

## 6. Decisions that belong to the user, not to you

Do not settle these unilaterally. They were raised and are unanswered:

- **Non-root user in WSL?** Cheap to fix now, annoying later.
- **Does the Python side live in WSL too, or stay on Windows?** Setup above assumes WSL.
- **`CARGO_TARGET_DIR` on the WSL filesystem?** Only worth it if `/mnt/c` builds are slow.
- **Phase 2's "≥6 weeks paper"** — an invented number, not a derived one.
- **`§5.2` table sketch** — worth review before Phase 1 hardens the schema, particularly
  `positions` as a derived view over `fills` rather than stored truth.
- **Depending on `trading-journal/backtest` in place vs extracting it** — see §2 of the spec.

Also standing: the user asked *not* to archive the signum prompt files in this repo.

---

## 7. Two lessons this session paid for

**Bytes, not text.** Contract artifacts are written with `plan.canonical_bytes` and
`Path.write_bytes`, never `write_text`. Text mode translates newlines per-platform, and
`read_text` translates them back on the way in — so a round-trip test passes on the very machine
producing the wrong bytes. `planner/tests/test_fixture.py` now compares bytes and asserts no `\r`
anywhere. `.gitattributes` marks the fixture and schema `-text` so git never rewrites them.

**Build the real thing, not a stub.** The turnover limit was specified as a risk-gate veto. The
first run of the real pipeline was *rejected*, because going flat → 75% invested simply is 75%
turnover, so no deployment could ever make its first trade. That produced a genuine design change
(turnover is a budget in the diff, not a limit in the gate — spec §6). A trivial placeholder
constructor would have passed the gate and hidden it.

---

## 8. Conventions that will bite if ignored

Full list in [design spec §12](./design-spec.md#12-conventions-that-matter). The ones that cause
silent wrongness:

- **`ts_utc` is always the bar OPEN.** Off by one hands the strategy a free look at the future.
  `planner/inspect.py` checks `open[t] == close[t-1]` as evidence; run `ai-trader data verify`.
- **A decision at `T` may only see bars that closed at or before `T`** (`pipeline.usable_horizon`).
- **Universe snapshots are never backfilled.** A universe reconstructed today and applied to
  history is survivorship bias. `universe.load` deliberately refuses to substitute a nearby date.
- **Money is `Decimal`, never `float`**, and crosses the language boundary as a *string*.
- **Fail closed.** Missing, stale or incomplete data means stop and alert. `GateFailure` is not
  something to catch and work around.
- **Disclose before reporting.** Unenforced limits, degenerate features and an uncalibrated cost
  model print above the numbers, never below.
- **No silent caps.** Anything the turnover budget defers is disclosed on the plan.

---

## 9. Orientation

- [`design-spec.md`](./design-spec.md) — the authoritative document. §0 is non-negotiable, §3.1 is
  the run loop, §7 is where AI is and is not, §11 is prior art reviewed.
- `planner/src/planner/pipeline.py` — the decision path end to end; read this first for code.
- `schema/plan.schema.json` — the contract between the two languages.
- `../trading-journal/` — separate project, the user's manual futures journal. Its `backtest/`
  package is the validation harness this project will depend on at Phase 1. Do not modify it.
