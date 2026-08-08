# Live UI checks (Playwright)

    bun run test:ui

Not hermetic. These launch a browser against a **running api and real
bots**, and click the real buttons — which is the only way to catch what
kept slipping through: copy that no longer matched behaviour, a control
verb that was disabled when it shouldn't be, and how long a bot actually
takes to obey Stop. Every assertion here corresponds to something that
shipped wrong.

Needs the api on :7434 (override with `API_BASE`).

`stop-latency.spec.ts` stops and restarts **futures-noise**. It asserts the
book is flat first and restores running at the end — but it does press Stop
on a live bot, so don't run it with a position open.

`controls.spec.ts` drives **crypto-portfolio**, which has no process running
it. That is deliberate: a control nothing is listening for must not look
like one that was obeyed, and this is the bot where that is true.

One worker, no retries, on purpose: a retry would press Stop a second time
and paper over exactly the flakiness worth knowing about.
