"""The cross-repo one-code-path gate for the futures bot.

Replays a fixed window through the FULL stack (journal engine -> BookRuntime)
and compares per-sleeve trade counts and net dollars against the committed
fixture. Drift on either side of the repo boundary fails here first —
the same honesty mechanism as ai-trader's plan-schema round-trip fixture.

Regenerate deliberately (after an INTENDED strategy change, with the
change's own evidence recorded in the journal repo's ledger):
    REGENERATE=1 ./run.sh parity
"""

import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
JOURNAL = (HERE / "../../../trading-journal/backtest").resolve()
FIXTURE = HERE / "parity-fixture.json"
WINDOW = "2026-06-01"


def run_replay() -> dict:
    # File mode on purpose: this is a hermetic gate against a committed
    # fixture, not a deployment — it must neither write fixture-window rows
    # into the shared records DB nor depend on one being reachable.
    env = {k: v for k, v in os.environ.items() if k != "DATABASE_URL"}
    subprocess.run(
        [str(JOURNAL / ".venv/bin/python"), "-m", "backtest.cli", "bot-replay",
         "--rules", "rules-lab", "--start", WINDOW],
        cwd=JOURNAL, check=True, capture_output=True, env=env,
    )
    rows = [json.loads(l) for l in (JOURNAL / "botstate/journal.jsonl").read_text().splitlines()]
    out: dict = {}
    for r in rows:
        s = out.setdefault(r["sleeve"], {"n": 0, "net": 0.0})
        s["n"] += 1
        s["net"] = round(s["net"] + r["dollars"], 2)
    return {"window_start": WINDOW, "sleeves": out}


def main() -> int:
    got = run_replay()
    if os.environ.get("REGENERATE") == "1":
        FIXTURE.write_text(json.dumps(got, indent=2, sort_keys=True) + "\n")
        print(f"fixture regenerated: {FIXTURE}")
        return 0
    want = json.loads(FIXTURE.read_text())
    if got == want:
        print("PARITY OK —", ", ".join(
            f"{k}: n={v['n']} ${v['net']:+,.0f}" for k, v in sorted(got["sleeves"].items())))
        return 0
    print("PARITY DRIFT:", file=sys.stderr)
    print("  expected:", json.dumps(want, sort_keys=True), file=sys.stderr)
    print("  got:     ", json.dumps(got, sort_keys=True), file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
