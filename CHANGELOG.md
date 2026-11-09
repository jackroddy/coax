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
  picker names the file to create. The picker shares the session screen's header
  and marks the chosen row with `›` and a shaded background.
- Started inside a decade pane, coax joins the decade socket. The status
  line says when it is connected.
- Permission requests from the agent open a box over the conversation, with
  the options numbered; a digit answers, Esc cancels. The answer stays in
  the conversation as a dim `⎿` line. `--allow` answers every
  request with the first allow option instead, for scripted runs.
- Ctrl+t shows or hides the plan, as a list above the prompt. It is hidden
  until then.
- `--prompt <text>` sends a first prompt as soon as the session opens.
- When `DECADE_MCP` is set, the agent is given decade as an MCP server in the
  session, so it can drive decade's panes itself.

### Changed

- The session has no panes. A header names the agent (by its config entry when
  it has one), whether it is working,
  decade when connected and the session, and the conversation fills the width
  under it. Your prompts sit on a shaded row behind `>`, the agent's text
  starts with `●` and its thoughts with a dim `✻`. A tool call is two rows: a
  dot coloured by status, the kind and the title, then the location, the size
  of any edit or the status. A title too long for its row is cut with `…`.
- The prompt sits between two rules and shows three rows. It wraps at word
  breaks and grows with the text up to half the screen. The footer under it
  lists the keys that work in the current state and, when the agent has sent a
  plan, how many steps are done.
- The mouse wheel scrolls the conversation three lines, so coax now captures
  the mouse and the terminal's own text selection needs Shift held. PageUp and
  PageDown move a full page, and End jumps to the bottom. While scrolled up,
  new output leaves the view where it is and the last row says how many lines
  are below.
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
