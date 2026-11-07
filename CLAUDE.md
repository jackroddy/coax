# chat-tui

A single-crate cargo project: a terminal chat that drives an agent over the
Agent Client Protocol (ACP). The agent is a subprocess speaking JSON-RPC on
its stdin and stdout; this program is the ACP client and owns the screen. The
name `chat-tui` is a placeholder until Jack picks one.

Design rationale that the code does not state lives in foam's memories, and
pending work lives in foam's issues. `foam prime` prints both at session start.
Until something runs, the README stays at its header and license, and design,
plans and decisions go to foam, never to this file.

## conventions

**Formatting.** Stock rustfmt, with no configuration. There is no
`rustfmt.toml`. Do not add one: the code is written against rustfmt's defaults.

**Comments.** Follow the `rust-comments` skill, and load it before writing or
editing a comment rather than reconstructing the rules from memory. The default
is no comment.

**Tests.** Unit tests stay inline in the file they cover. Integration tests
under `tests/` drive the session model against the fake agent in
`src/bin/fake-agent.rs`, which replays a scenario file; every ACP event the
program handles has a scenario that exercises it. Test names are full
sentences describing the property, not `test_foo`.

**Branches.** Work is committed on `dev`, in batches. A milestone or issue that
changes the command line, the keys or the scenario format, and will not be
finished before the next release, goes on a branch named for it. That branch
merges into `dev` once the work is whole, and takes merges from `dev` when the
two drift apart. Nothing else branches off `dev` unless Jack asks. `main` takes
a merge from `dev` only at a release.

**Releases.** Jack says when a release is, never Claude. Until he does, the
version in `Cargo.toml` stays where it is and changelog entries accumulate
under `## [Unreleased]` with `### Added` / `### Changed` / `### Removed`
headings and no date. The version heading, the date, the `Cargo.toml` bump and
the compare link at the foot of the changelog are all part of the release
commit. Release tags are annotated and named `v<version>`. Publish from `main`
after `dev` merges in, with a clean tree.

**Versions.** What is versioned is the command line, the keys, and the
scenario format. Under cargo's rule that 0.x's minor is the breaking axis, a
change that breaks any of them moves the minor. Anything else moves the patch.

**Seeing the screen.** The program is a TUI, so a session checks it by
running it inside a detached tmux and reading the pane back:

```console
$ tmux new-session -d -s chat -x 100 -y 30 "target/debug/chat-tui target/debug/fake-agent scenarios/hello.json"
$ tmux send-keys -t chat 'hello' Enter
$ tmux capture-pane -t chat -p
$ tmux kill-session -t chat
```

**The fake agent.** `fake-agent <scenario.json>` is an ACP agent that plays
one act of the scenario per prompt. A step is `{"update": <session update as
on the wire>}`, `{"permission": {...}}`, `{"delay_ms": N}` or `{"stop":
"end_turn"}`. `scenarios/hello.json` is the reference; tests include it.

## checks

```console
$ cargo fmt --all
$ cargo fmt --all --check
$ cargo clippy --all-targets --all-features
$ cargo test --all-features
```
