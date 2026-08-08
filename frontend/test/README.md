# Live UI checks

Not unit tests: these launch the real browser against a running api and
click the real buttons. They exist because "it compiled" and "the copy is
right" turned out to be different claims — twice — and because the only
way to know how fast a control is obeyed is to press it and time it.

    bun run frontend/test/test_controls.ts       # verbs, copy, enablement
    bun run frontend/test/test_stop_latency.ts   # unapplied stop → red; stop latency

Both need the api on :7434 and a Chromium at the path in `uidrive.ts`
(Playwright's cached download is fine — no Playwright package needed;
`uidrive.ts` speaks CDP directly).

`test_stop_latency.ts` stops and restarts **futures-noise**. It asserts the
bot is flat before doing so and restores it to running at the end, but do
not run it with a position open.
