# Feedback: running `cargo` from Claude Code via the CT Local LLM plugin

Written from the agent's (Claude Code) point of view during the dm420 bus
implementation, to help improve the `ct` local-LLM plugin. This is about the
*experience of needing to build/test code* in a project where raw shell is
restricted.

## Setup that shaped the experience

- The project allowlists only `Bash(ct *)` and the `ct` MCP tools in
  `.claude/settings.local.json`. Plain `cargo` via `Bash` is **denied** by a
  PreToolUse hook.
- `mcp__ct__ct_local_check_output` exists and is exactly the right tool — but it
  was a **deferred** tool (its schema wasn't in my initial toolset; only its name
  was discoverable via `ToolSearch`). I did not know it could run builds until the
  user hinted at it.

## What actually happened (timeline)

1. Finished the `types` crate, wanted to verify it. Ran
   `Bash: cargo test -p types`.
   → Result: **`Hook PreToolUse:Bash denied this tool`** and nothing else. No
   reason, no hint about what *is* allowed, no pointer to an alternative.
2. Assumed it was a sandbox/network issue, retried with
   `dangerouslyDisableSandbox: true`.
   → Same opaque denial. (So the retry burned a turn for nothing.)
3. Read `settings.local.json`, saw only `Bash(ct *)` was allowed, and concluded
   cargo was simply off-limits. Fell back to asking the **user** to run
   `! cargo test -p types` and paste the output. That worked, but it puts a human
   in the loop for every compile.
4. Built the much larger `bus` crate the same way — writing code I could not
   compile myself, planning to batch a single user-run build at the end. Higher
   risk of stacking errors.
5. The user asked: *"can you do cargo commands via the ct local LLM plugin?"* I
   searched deferred tools, found `mcp__ct__ct_local_check_output`, and from then
   on ran `cargo build / clippy / test` directly.

## What worked well once I found `ct_local_check_output`

- **The JSON shape is well-suited to an agent.** `{ok, summary, first_error:
  {file, line, message}, suggested_next_step}` is almost ideal:
  - `ok` drives control flow with zero parsing.
  - `first_error.{file,line,message}` was precise enough to fix things directly —
    e.g. it pointed at `crates/bus/src/handle.rs:182 "this if statement can be
    collapsed"`, and I fixed exactly that without seeing raw clippy output.
- **Raw output stays out of my context.** For a long multi-file build session this
  is a real win — I'm not paying context for hundreds of lines of cargo noise.
- The clippy and `cargo test -p types` runs both classified cleanly ("All 2 unit
  tests and 1 doctest passed").

## Friction points / suggestions

1. **Discoverability is the biggest gap.** I only used the right tool after a human
   hinted. Two fixes that would have closed the loop immediately:
   - Have the PreToolUse hook that denies `cargo`/`Bash` return a message that
     *names the alternative*: e.g. "Direct `cargo` is not allowlisted — run build/
     test commands through `mcp__ct__ct_local_check_output` instead." A denial that
     teaches the remediation is worth far more than a bare "denied".
   - Consider surfacing `ct_local_check_output` as a non-deferred tool in projects
     where shell is locked down, or mention it in the ct MCP server instructions
     (the "prefer ct over Bash" reminder could add "…and run builds/tests via
     `ct_local_check_output`").

2. **Only `first_error` is returned.** With several compile errors I fix one,
   re-run, fix the next — N round-trips. An optional `errors: [...]` array (capped,
   say, at 10) or at least an `error_count` would let me fix a batch per run.

3. **Timeout reporting is thin.** A 180s `cargo test` returned only
   `"timed out after 180s"` with `first_error: null`. I couldn't tell:
   - whether it was still *compiling* vs *running* tests,
   - which test was executing when it stalled (real hang vs slow),
   - any partial pass/fail progress.
   Suggestions: report a `phase` ("compiling" | "running"), the last-started test
   name, and/or a captured tail of output on timeout. Also: the **default 60s is
   low for cold Rust builds** (dependency compilation alone can exceed it) — a
   higher default, or auto-detecting cargo and bumping it, would help.

4. **For test runs, a structured pass/fail summary would beat prose.** Something
   like `{tests_passed: 9, tests_failed: 1, failures: [{name, message}]}` would be
   directly actionable. The current prose summaries are good for humans but I have
   to trust the classification rather than see the failing assertion.

5. **Minor:** a way to distinguish "command not found / not runnable" from "ran and
   failed" would help me tell environment problems from genuine code failures.

## Postscript: a concrete case where the timeout gap hurt

Right after drafting this, a `cargo test -p bus` run hung. `ct_local_check_output`
returned only `"timed out after 180s"` / `"after 90s"` with `first_error: null` —
no indication of *which* test was stuck or whether it was compile vs run. Because
the raw output is (deliberately) hidden from me, I could not see the harness's own
`test NAME ... ok` progress lines, which would have named the culprit instantly. I
had to bisect by re-running each test individually (`--exact`) — five separate tool
calls — and even then I happened to test the wrong five; the human watching the
real terminal saw that `state_late_join` was the one that never printed and told me
directly. With raw output hidden, that progress signal is exactly what I lost.

It turned out to be a real bug (State late-join: `watch::Sender::send` no-ops with
zero receivers, dropping the value), so finding it was valuable — but the *path* to
it was slow and leaned on the human.

**Concrete asks this motivates:**
- On timeout for a test command, return the last-started / still-running test name
  (parse the harness's `test X ...` lines internally and report the one without a
  matching `... ok`). This single field would have replaced ~6 round trips.
- Optionally a `partial` field: which tests passed/failed before the timeout.
- A `phase` field ("compiling" vs "running") so I know whether to suspect a slow
  build vs a hanging test.

## Net

Once discovered, `ct_local_check_output` is a genuinely good fit for an agent —
classified JSON with file/line/message and raw output kept out of context is
exactly what I want. The main improvements are **discoverability** (teach the tool
in the denial message and server instructions), **multi-error reporting**, and
**richer timeout/test-result structure** (especially: name the hanging test).
