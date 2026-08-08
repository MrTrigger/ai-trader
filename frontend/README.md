# TriggerTrader frontend

React + TypeScript + Vite + Tailwind, per `docs/design-spec.md` §"Preference:
Rust in the backend, React + TypeScript in the frontend" — the same stack
`trading-journal` maintains.

    ../bin/dev.sh   # both halves, both reloading: vite HMR on :5174 and
                    # cargo-watch rebuilding the api on :7434

    bun install
    bun run dev     # :5174 alone, HMR, proxies /api to the api on :7434
    bun run build   # tsc --noEmit && vite build -> dist/

Work on :5174 while editing the UI (instant), and open :7434 to see what a
deployment actually serves. The api resolves `frontend/dist` per request, so
a rebuild goes live without restarting it.

The api serves `frontend/dist` when it exists (auto-detected; `--static-dir`
overrides), so a built bundle needs no flags. Until this port covers every
screen, an api started without a built bundle falls back to the legacy
single-file page.

## What the design is doing

The console answers one question before any other — *is everything as I left
it?* — so the fleet leads with STATE (how many bots, how many sending orders,
how many halted, whether any real money is exposed) and money second. A
portfolio app opens with a P&L number; an operations console opens with
control.

Two motifs carry it:

* **The route chain** — broker → account → money → execution, drawn as one
  connected path, because that is the path an order takes. Presented as four
  separate boxes, the page could say "paper" in one and "live" in another and
  be telling the truth twice. As stations on a line they cannot be read
  independently.
* **The kill rail** — distance to the threshold that stops the book.
  Deliberately not a progress bar: progress fills toward something you want.

Colour carries meaning, not decoration: green is running, coral is alarm, and
**amber is consequence** — real money is not "bad", it is serious, and it gets
its own hue rather than borrowing the alarm colour.
