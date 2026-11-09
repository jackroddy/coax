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
  picker names the file to create. The picker opens with the same
  first line and rule as the session header, and marks the chosen row with `›`
  and a shaded background.
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
  block wider than the box shows a window onto it, with a scroll bar row
  under it. The bar is a dim `─` track and a `━` thumb whose length and place
  follow the visible share and the pan, in the accent colour while the
  pointer is over it or dragging it. Dragging the thumb sideways pans the
  block with the pointer, and a click on the track moves the thumb there.
  Shift with the wheel, or a horizontal wheel, over the block pans it four
  columns at a time.
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
- The header's second line shows the session's settings as plain text, such
  as `mode auto   model opus   effort default`, in the order mode, model,
  thought level, other model settings, then the rest. The name is dim and the
  value is in the normal colour.
  A click on a chip opens a box under it that lists the choices with their
  descriptions and marks the current one. A click or the arrow keys with Enter
  applies the choice through `session/set_config_option`, Esc or a click
  elsewhere closes the box, and an error from the agent shows in the box. A
  yes or no setting flips on a click. The mode value is yellow in `auto` and
  `bypassPermissions`, where the agent does not ask. A
  value turns the accent colour under the pointer. When the settings do not
  fit, they move from the right into a dim `⋯` that lists them. An agent with no config options but with modes
  gets a mode chip through `session/set_mode`.
- The header's first line also shows the context as `ctx 28k/1.0M 3%`, yellow past 80% and
  red past 95%, and the session's tokens as `↑28k ↓4` with the cost after
  them, such as `$0.14`, when the agent sends one. On a narrow screen the
  tokens go first, then the context and the plan summary.
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
- Commands in the prompt. A prompt that starts with `/` is a coax command and
  turns cyan. There is one per header chip, named after the setting in
  lowercase with spaces as `-`, such as `/mode`, `/model`, `/effort` and
  `/fast`. Enter opens that chip's box, or flips a yes or no setting, and the
  command never sends anything to the agent. `/model` waits while the agent
  works, as the chip does. A prompt that starts with `//` is one of the
  agent's slash commands and turns green. `//compact` sends `/compact` after a
  box that names the command and its description asks for Enter, and Esc backs
  out. Text after the name goes with the command, so `//review the parser`
  sends `/review the parser`.
- While a command's name is being typed, a list above the prompt shows the
  matching commands with their descriptions: coax's for `/`, and the ones the
  agent advertises through `available_commands_update` for `//`. Up and Down
  move in it, Enter on an entry fills it into the prompt and a second Enter
  runs it, and Esc closes the list.
- The scenario format gains an optional top-level `commands` array, the slash
  commands the fake agent advertises when a session opens.
- Ctrl+t shows or hides the plan, as a list above the prompt. It is hidden
  until then.
- `--prompt <text>` sends a first prompt as soon as the session opens.
- When `DECADE_MCP` is set, the agent is given decade as an MCP server in the
  session, so it can drive decade's panes itself.

- Every fenced code block, with or without a language, has a copy glyph in
  its top-right corner. It is dim, turns the accent colour under the pointer,
  and a click puts the block's whole text on the clipboard through an OSC 52
  escape sequence. The glyph then reads `copied` for a second. Your terminal
  has to allow OSC 52, and under tmux `set-clipboard` has to be `on`.
- A click on an edit call shows its diff under it: removed lines in red after
  a `-`, added lines in green after a `+`, and the unchanged lines around
  them dim. A click on a command call shows its output, laid out as markdown.
  Both are folded until clicked, and a second click folds them again. Each
  expandable call, subagent calls included, ends its title row with a dim `▸`
  when folded and `▾` when expanded. The marker turns the accent colour while
  the pointer is over the call.
- `scenarios/blocks.json` plays an agent message with code blocks, an edit and
  a command with output, for the fake agent.

- A conversation taller than the screen has a scroll bar in the last column
  of the transcript, which is then one column narrower. The track is a dim
  `│` and the thumb a `┃` sized to the visible share, at the bottom when you
  are at the latest message. The thumb is in the accent colour while the
  pointer is over it or dragging it. Dragging it scrolls with the pointer,
  and a click on the track centres the thumb there. The wheel, PageUp,
  PageDown and End still scroll.
- Enter on a `/name` or `//name` that matches no command leaves the text in the
  prompt and shows a dim `no command /name` (or `no agent command //name`)
  in the rule under the prompt until the next key.

### Changed

- The session has no panes and reads like a messaging thread. The header is
  three rows. The first has `coax` and the agent's name (by its config entry
  when it has one) on the left. On the right it has the plan summary, context,
  tokens, cost and decade when connected, dim and joined by `·`, and then
  whether the agent is working, with a spinner stepping through `◐◓◑◒`. The
  second is the settings and the third a dim rule. Your messages sit in a rounded box
  in the accent colour against the right edge, at most 70% of the width. The
  agent's text sits in a blue rounded box on the left, at most 90% of the
  width. Thoughts start with a dim `✻` and stay unboxed with a 10% margin on
  the right, as do tool calls. A tool call is two rows: a dot coloured by
  status, the kind and the title, then the location, the size of any edit or
  the status. A done call with nothing to show but its title has no second
  row. A title too long for its row is cut with `…`. Your answer to a
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
