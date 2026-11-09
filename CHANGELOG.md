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
- A `[palette]` table in `config.toml` sets the colours, under omarchy's
  names: `accent`, `muted`, `selection`, `foreground`, `red`, `yellow`,
  `orange`, `green`, `cyan`, `blue` and `magenta`. A value is `"#RRGGBB"` or an
  ANSI colour name. A name left out keeps its ANSI colour, and without
  `muted` dim text uses the terminal's dim attribute. A bad value is an error
  naming the file.
- Wide code blocks and tables in the agent's text keep their full width, where
  before a code line was cut with `…` and a table was squeezed or stacked. A
  block wider than the box shows a window onto it, with a dim `◂` or `▸` at
  the edge that has more. Shift with the wheel, or a horizontal wheel, over
  the block pans it four columns at a time.
- A bare `coax` starts every configured agent at once and lists their earlier
  sessions for the current folder in a `sessions` pane above the agents,
  newest first, each with its title, its agent and its age. j and k move
  through the sessions and then the agents. Enter on a session loads it
  through `session/load` and shows its history, and Enter on an agent starts
  a new session as before. Only agents that advertise `session/load` and
  `session/list` contribute sessions. A session is chosen only in the picker,
  never from inside a conversation. `--agent` and a raw command start a new
  session without listing.
- The scenario format gains an optional top-level `sessions` array. Each entry
  has an `id`, a `title`, an `updated_at` timestamp and a `history` of
  `update` steps. With it, the fake agent advertises `session/list` and
  `session/load`, lists the sessions and replays a session's history when it
  is loaded. `scenarios/resume.json` is an example.
- Ctrl+t shows or hides the plan, as a list above the prompt. It is hidden
  until then.
- `--prompt <text>` sends a first prompt as soon as the session opens.
- When `DECADE_MCP` is set, the agent is given decade as an MCP server in the
  session, so it can drive decade's panes itself.

### Changed

- The session has no panes and reads like a messaging thread. A header names
  the agent (by its config entry when it has one), whether it is working,
  decade when connected and the session. Your messages sit in a rounded box
  in the accent colour against the right edge, at most 70% of the width. The
  agent's text sits in a blue rounded box on the left, at most 90% of the
  width. Thoughts start with a dim `✻` and stay unboxed with a 10% margin on
  the right, as do tool calls. A tool call is two rows: a dot coloured by
  status, the kind and the title, then the location, the size of any edit or
  the status. A title too long for its row is cut with `…`. Your answer to a
  permission request is the choice in the accent colour on the right, and
  notices are centred and dim.
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
