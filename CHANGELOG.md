# Changelog

All notable changes to coax are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not. What is versioned is the
command line, the keys, and the scenario format the fake agent reads.

## [Unreleased]

### Added

- Named agents in `~/.config/coax/config.toml` (under `$XDG_CONFIG_HOME` when
  it is set). Each `[agents.<name>]` entry has a `command`, `args` and `env`.
  `coax --agent <name>` starts that entry.
- A picker over the named agents. It opens on a launch with no agent named
  and no command. Up and Down (or `k` and `j`) move, Enter starts the
  highlighted agent, and Esc or Ctrl+c quits. `default = "<name>"` in the
  config sets which agent starts highlighted. With no agents configured, the
  picker names the file to create.
- Started inside a decade pane, coax joins the decade socket. The status
  line says when it is connected.
- Permission requests from the agent pause on screen above the input, with
  the options numbered; a digit answers, Esc cancels. `--allow` answers every
  request with the first allow option instead, for scripted runs.
- `--prompt <text>` sends a first prompt as soon as the session opens.
- When `DECADE_MCP` is set, the agent is given decade as an MCP server in the
  session, so it can drive decade's panes itself.

### Changed

- Your prompts start with `›`, and the agent's thoughts sit behind a dim `│`
  gutter. Consecutive tool calls group under one `✦ N tools` line, with one
  line per call: a status glyph, the kind, the title, and the size of any
  edit. A line too long for the pane is cut with `…`, so the status stays
  visible.
- The screen has a header and a footer in place of the reverse-video status
  bar. The header names the agent by its config entry when it has one. It
  also shows whether the agent is working, decade when connected, and the
  session. The conversation and the prompt sit in rounded panes. The footer
  lists the keys that work in the current state.
- Agent text is drawn as markdown: headings, lists, tables, rules, inline
  code and fenced code with highlighting.
- A bare `coax` opens the agent picker in place of printing usage. A flag
  coax does not know is a usage error, where before it was taken as the agent
  command.

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
