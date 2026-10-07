## Agent skills

### Issue tracker

Issues live in GitHub Issues on the fork, `ALLiDoizCode/halo-ce-universal` (never the upstream `cybersecurity/halo-ce-universal`); pass `--repo ALLiDoizCode/halo-ce-universal` to every `gh` call. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles use their default label names (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root, created lazily. See `docs/agents/domain.md`.

## The AFK factory

`ready-for-agent` is the factory's queue (`docs/agents/triage-labels.md`). When an issue
carries it and its blockers are closed, `.github/workflows/agent-implement.yml` runs
`.sandcastle/agent-implement-issue.ts`, which runs `/mattpocock-skills:implement`, then
`/mattpocock-skills:code-review` against `main` in a fresh session, then the gate, then
pushes `sandcastle/issue-N` and opens a PR labelled `ready-for-human`. A failed run moves
the issue to `needs-triage`. Specs, blocked issues, issues with an open PR and any issue
labelled `wayfinder:*` are skipped (`.sandcastle/ready-issues.ts`). A human merges every PR.

The sandbox has no game data, so it cannot make a measurement on the real maps or in the
running game. When a ticket's acceptance includes one, the agent proves what the tests can,
and opens a `Measure: ...` issue labelled `ready-for-human` with the command and the criteria
still to be shown (`.sandcastle/implement-prompt.md`). The pull request closes the ticket
when it merges; the measurement issue stays open until a human has made the run.

**The gate reads its steps from CI.** The gate is the `run:` steps of the `gate` job
(else the `checks` job) of `.github/workflows/ci.yml` **on `main`**, in order. If there is
no `ci.yml`, no such job, or no runnable step, the gate runs nothing and logs that loudly.
Today the gate is the `gate` job of `ci.yml`: the Rust workspace in `rust/` is installed
(`Toolchain`), formatted and linted (`Format and lints`), tested (`Test`) and built for
32-bit x86 and WebAssembly. Its steps are a copy of the `rust` job in `build.yml`; keep the
two the same. The C game and the crates outside the workspace are not gated.
The rule lives in one place, `gateFromCi` in `.sandcastle/run-gate.ts`, tested by
`.sandcastle/run-gate.test.ts`.

The runner's own commands, from `.sandcastle/`: `npm ci`, `npm test`, `npm run typecheck`.
The runner's only Node manifest is `.sandcastle/package.json`, separate from anything at
the repository root. The sandbox image is the shared
`ghcr.io/toon-protocol/sandcastle-agent`; this repo has no Dockerfile for it.
`close-linked-issues.yml` is identical in every factory repo. Its source is
`templates/factory/` in `toon-protocol/toon-meta`, so change it there first.
