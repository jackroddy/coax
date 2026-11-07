# Changelog

All notable changes to coax are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not. What is versioned is the
command line, the keys, and the scenario format the fake agent reads.

## [Unreleased]

## [0.1.0] - 2026-11-06

The name on crates.io, and a first session.

### Added

- A terminal chat that runs an ACP agent as a subprocess, sends prompts, and
  shows the session: agent text, thoughts, tool calls with locations and diff
  summaries, permission decisions, and the plan. Enter sends, Esc cancels,
  Ctrl+c quits, PageUp and PageDown scroll.
- `coax-fake-agent`, an ACP agent that replays a scenario file, for tests and
  for development without a real agent.

[0.1.0]: https://github.com/jackroddy/coax/releases/tag/v0.1.0
