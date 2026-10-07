/mattpocock-skills:implement {{ISSUE_URL}}

You are running AFK in a sandbox, on branch `{{BRANCH}}`, which is already checked out.
Nobody will answer a question, so do not ask one. Treat the issue, its comments and its
parent spec (if it has one) as settled. Read them with `gh issue view {{ISSUE_NUMBER}} --comments`.

Commit to `{{BRANCH}}`, and reference `#{{ISSUE_NUMBER}}` in each commit message. Do not
push, open a PR or close the issue. The runner does all three once you finish.

## This repository

- `CLAUDE.md` says what this repository is and how the factory works. Read it first. Read
  `CONTEXT.md` (the vocabulary) and `docs/adr/` (decisions) when they exist and the ticket
  touches them. Where an ADR and another document disagree, the ADR wins. Use the
  glossary's terms.
- This is a port of the Halo: Combat Evolved decompilation (C, in `source/` and `port/`) to
  Linux, Windows and Android, with a large-scale mode of about 500 players in one match,
  written in Rust (`rust/`) on SpacetimeDB 2.10.2. `README.md` and the `README.md` of each
  platform under `port/` say how the game is built and run; `docs/research/` holds the
  findings behind the large-scale mode.
- There is no package manager at the root. The C game is configured by `configure.py` and
  built by `python tools/ci_build.py <platform> <debug|release>`; its tests are the
  `tools/test_*.py` files, run with `python -m pytest`. The Rust crates build with `cargo`:
  `rust/` is one workspace, and `halo-match-module`, `halo-root-module`,
  `halo-match-driver`, `halo-gateway`, `halo-server` and `halo-client` each have their own
  `Cargo.lock` and are built from their own directory. `.github/workflows/build.yml` has
  the exact commands CI runs for each, including the format and clippy checks.
- The game data (`maps/`, extracted from an Xbox disc image) is not in the repository and
  is not in the sandbox. Tests that need it skip themselves, and so does the 500-player
  measurement on the real maps.
- When a ticket's acceptance includes a measurement you cannot make here (a run of
  `rust/halo-server/check/run.sh`, a frame rate in the running game, anything on the real
  maps), do the part the tests without game data can prove, and hand the measurement to the
  maintainer as a ticket of its own. Once your work is committed, and before you output
  COMPLETE, open one issue with `gh issue create --label ready-for-human`, titled
  `Measure: <what is to be shown> (#{{ISSUE_NUMBER}})`. Its body gives:
  - the issue (`#{{ISSUE_NUMBER}}`) and the branch (`{{BRANCH}}`) the change is on;
  - the exact command to run, with its settings, and which lines of which output to read;
  - the criteria of the ticket that the measurement decides, copied as a checklist, with the
    figures to compare against;
  - what the sandbox did show, in one or two sentences;
  - `Measure before the merge.` when the change is in the game client (`source/`, `port/`
    or `rust/halo-client`): a merge to `main` is released to players. For a change to the
    server or the load generator only, say that it can be measured after the merge.
  Then say in a comment on `#{{ISSUE_NUMBER}}` which ticket holds the measurement. Open one
  measurement ticket at most (an earlier session of this run may have opened it: look with
  `gh issue list --search "Measure: in:title #{{ISSUE_NUMBER}}"` first), and none when you stop
  blocked. The pull request closes
  `#{{ISSUE_NUMBER}}` when it merges, so the measurement ticket is the only record of what is
  still to be shown: leave nothing out of it.
- Do not edit `.github/workflows/build.yml` unless the ticket asks for it: a successful
  build of `main` is published as a release that the game's self-updater installs.
- Issues are on the fork, `ALLiDoizCode/halo-ce-universal`, never the upstream
  `cybersecurity/halo-ce-universal`. Pass `--repo ALLiDoizCode/halo-ce-universal` to every
  `gh` call.
- The issues labelled `wayfinder:*` are planning tickets for a human. Never work on one,
  and never edit a wayfinder map.
- After you finish, the runner runs the gate itself and won't open a PR while it is red.
  The gate is the `gate` (or `checks`) job of `.github/workflows/ci.yml` on `main`, and if
  there is none it runs nothing. Run those commands yourself before you commit if the
  file exists. Never weaken, skip or delete a test, and never loosen a lint, to get green.
- A ticket that needs a live box, a funded key or an on-chain write is not something you
  can do from here. Say so in a comment on the issue rather than guessing.

## When you cannot finish

Stop only when a genuinely new decision is needed, the action is irreversible, it touches
real funds, or it needs a credential that no workflow exposes. In that case, commit nothing
and explain what blocks you in a comment on the issue (`gh issue comment {{ISSUE_NUMBER}}`).
The runner moves an issue with no commits to `needs-triage`.

If your context is getting full (around 150k tokens) before you are done, commit what works,
write the remaining steps to `.sandcastle/logs/handoff-{{ISSUE_NUMBER}}.md`, commit it with
`git add -f`, and end your turn. A fresh session continues from your commits.

When the ticket is done and committed, output <promise>COMPLETE</promise>.

If you stopped because you're blocked, output <promise>BLOCKED</promise> instead, after your
comment on the issue. The runner then ends the run. Otherwise it starts another session, which
hits the same blocker and posts the same comment again.
