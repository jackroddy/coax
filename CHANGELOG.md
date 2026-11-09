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
- The header shows the session's settings as chips after the agent's name, in
  the order mode, model, thought level, other model settings, then the rest.
  A click on a chip opens a box under it that lists the choices with their
  descriptions and marks the current one. A click or the arrow keys with Enter
  applies the choice through `session/set_config_option`, Esc or a click
  elsewhere closes the box, and an error from the agent shows in the box. A
  yes or no setting flips on a click. The mode chip turns yellow in `auto` and
  `bypassPermissions`, where the agent does not ask. A chip is underlined
  under the pointer. When the chips do not fit, they move from the right into
  a `⋯` chip that lists them. An agent with no config options but with modes
  gets a mode chip through `session/set_mode`.
- The header also shows the context as `ctx 28k/1.0M 3%`, yellow past 80% and
  red past 95%, and the session's tokens as `↑28k ↓4` with the cost after
  them, such as `$0.14`, when the agent sends one. On a narrow screen the
  session id goes first, then the tokens, the context and the plan summary.
- The scenario format gains an optional top-level `config_options` array (the
  settings the agent offers, as ACP config options) and `modes`, and an
  optional `usage` on an act, which the prompt response reports. The fake
  agent answers `session/set_config_option` and `session/set_mode`.
- A subagent's tool calls and text group under the tool call that started it.
  The block shows folded as `● agent <description>  N tools`, with its status
  below, and a click on it shows what the subagent did, indented under it.
  Text from a subagent no longer joins the main agent's reply, and the main
  agent's later text no longer joins the subagent's. coax reads the Claude
  adapter's `_meta.claudeCode.parentToolUseId` tag to tell which call a
  subagent's output belongs to. coax declares the `subagent-transcript` client
  capability, which the adapter needs before it sends a subagent's text. The
  scenario format is unchanged: an `update`
  step already carries `_meta` as it does on the wire, and
  `scenarios/subagents.json` shows two parallel subagents.
- Choosing a different model after the first turn does not apply it at once.
  The model box shows, in yellow, that the next turn rereads the conversation
  through the new model, with its size when the agent has reported one, and
  without the cache. Choosing the same model again applies it. Choosing the
  current model or pressing Esc backs out. While the agent is working, the
  model chip is dim and a click on it does nothing. Other settings still
  switch at once.
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
- The prompt sits between two rules and shows one row. It wraps at word
  breaks and grows with the text up to half the screen. There is no footer
  row of key hints. When the agent has sent a plan, the header shows how many
  steps are done, before the working or idle state.
- Ctrl+c no longer quits at once in a session. While the agent is working it
  cancels the turn, as Esc does, and with text in the prompt it clears the
  prompt. Otherwise, including with the permission box open, it arms an exit
  guard for 1.5 seconds and a second Ctrl+c in that time quits. Ctrl+d on an
  empty prompt arms the same guard and a second Ctrl+d quits. Any other key
  disarms it. While armed, the rule under the prompt says which key to press
  again. The picker still quits on Esc and Ctrl+c at once.
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
