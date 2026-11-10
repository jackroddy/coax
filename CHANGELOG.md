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
  highlighted agent, and Esc or Ctrl+c quits. The highlight starts on the
  newest earlier session when there is one, and otherwise on the agent named
  by `default = "<name>"` in the config. With no agents configured, the
  picker names the file to create. The picker has a header of its
  own, `coax` and a title over a dim rule, and marks the chosen row with `›`
  and a shaded background.
- Started inside a decade pane, coax joins the decade socket. The status
  line says when it is connected.
- coax names its agent to decade in hello (the agent's name, its version and
  the session id) and reports on it. The state is `working` during a turn,
  `attention` with the tool call's title as the question while a permission
  request waits, `done` after a turn that ended normally and `idle` after a
  cancel. The activity is the title of the running tool call. The model,
  context use, session tokens and cost go along when they change. coax sends
  `ended` when it quits or the agent exits.
- A log of each run, one JSON Lines file in `$XDG_STATE_HOME/coax/logs`
  (`~/.local/state/coax/logs` when it is unset). The file is named by the
  start time and the process id and is readable by its owner only. coax keeps
  the newest 20 files. `COAX_LOG` sets how much is written: `off`, `info`
  (the default) or `debug`. `info` records the launch, the chosen agent and
  session, setting changes, slash commands run, the decade traffic, the
  agent's stderr and exit, every notice and error, panics with a backtrace,
  and how long the session took to open. `debug` adds every JSON-RPC line
  between coax and the agent. Keystrokes are never logged. If the file cannot
  be made, coax shows one notice and runs without a log.
- `/log` shows the path of the run's log file under the prompt.
- Permission requests from the agent open a box over the conversation, with
  the options numbered; a digit answers, Esc cancels. The answer stays in
  the conversation as a dim `⎿` line. `--allow` answers every
  request with the first allow option instead, for scripted runs.
- A `[palette]` table in `config.toml` sets the colours. It holds the
  terminal's slots under the names terminal themes use: `black`, `red`,
  `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, the same eight as
  `bright_black` to `bright_white`, `foreground` and `background`. Each takes
  `"#RRGGBB"` or an ANSI colour name, and a slot left out keeps the
  terminal's colour. With `background` set, coax paints every cell with it.
  coax's own slots take a terminal slot's name as well, and default to one:
  `accent` to `yellow`, `muted` (dim text and rules) and `selection` (the
  chosen row) to `bright_black`, and `orange` to `bright_red`. `tint`, a fill
  just off the background, mixes 7% of the foreground into the background.
  It takes their RGB from the config or, at startup, from the terminal's
  answers to OSC 10 and OSC 11, and falls back to `selection` when neither
  has them. A bad name or value is an error naming the file.
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
- A dim status line under the prompt shows the session's settings on the
  left, joined by `·`, such as `auto · opus · effort default · fast off`. The
  mode and the model show their value alone and the rest show the name and
  the value, in the order mode, model, thought level, other model settings,
  then the rest. A click on a setting opens a box above it that lists the
  choices with their descriptions and marks the current one. A click or the
  arrow keys with Enter applies the choice through
  `session/set_config_option`, Esc or a click elsewhere closes the box, and an
  error from the agent shows in the box. A yes or no setting flips on a click.
  The mode value is yellow in `auto` and `bypassPermissions`, where the agent
  does not ask. A value turns the accent colour under the pointer. When the
  settings do not fit, they move from the right into a dim `⋯` that lists
  them. An agent with no config options but with modes gets a mode setting
  through `session/set_mode`.
- A row above the status line shows the context in use as `context: 3%`
  and a bar of `▰` and `▱` across the rest of the row, in proportion. The
  percentage and the filled part turn yellow past 80% and red past 95%.
  Before the agent reports a context size, the row has no percentage and an
  empty bar.
- The status line also shows the session's tokens as `↑28k ↓4` with the cost
  after them, such as `$0.14`, when the agent sends one. On a narrow screen
  the model config settings and settings with no category move into `⋯`
  first. Then the tokens and cost go, then the plan summary, and then the
  remaining settings from the right.
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
  turns cyan. There is one per setting, named after the setting in
  lowercase with spaces as `-`, such as `/mode`, `/model`, `/effort` and
  `/fast`. Enter opens that setting's box, or flips a yes or no setting, and the
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
  Both are folded until clicked. A second click anywhere on the unfolded
  call folds it again, unless a link, a copy button or a scroll bar is
  under the pointer. Each expandable call, subagent calls included, ends its
  title row with a dim `▸` when folded and `▾` when expanded. While the
  pointer is over the call, the marker turns the accent colour and the
  whole call is shaded with the palette's `tint`.
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
  in the rule under the prompt until the next key. A prompt whose first word
  holds a second `/`, such as `/home/jack/x is broken`, is a path and sends
  as a normal message.
- The prompt has a cursor, and the terminal cursor is drawn at it, on
  wrapped rows too. Left and Right move by a character. Alt+Left, Alt+Right,
  Ctrl+Left, Ctrl+Right, Alt+b and Alt+f move by a word. Ctrl+a and Ctrl+e go
  to the start and end of the line, and Home and End to the start and end of
  the prompt. Delete removes the character after the cursor, Ctrl+w the word
  before it, and Ctrl+k everything from the cursor to the end of the line.
  Ctrl+u still clears the whole prompt.
- Alt+Enter inserts a newline in the prompt, and so does Shift+Enter in a
  terminal that reports it apart from Enter. Enter still sends. Each line
  wraps on its own. Up and Down move the cursor one drawn row and keep its
  column, whether the row above or below is part of the same line or not.
- Enter during a turn sends the prompt to an agent that declares
  `promptQueueing` under `claudeCode` in its capabilities, as the Claude
  adapter does, and the agent runs it once the running turn ends. For any
  other agent, coax holds the prompt and sends it when the turn ends. Until
  its turn starts, the prompt waits in a dim `queued` row above the working
  line, up to three rows with a count of the rest. Esc or Ctrl+c cancels the
  running turn and every queued prompt, and puts the queued text back in the
  prompt box. The fake agent declares `promptQueueing` and plays the prompts
  it is sent one after another.
- Pasting into the prompt inserts the text at the cursor with its newlines
  kept, and never sends it. coax turns on bracketed paste for this and turns
  it off again on exit, so a terminal without bracketed paste behaves as
  before.
- Text is measured in terminal columns. A CJK character or an emoji counts as
  two columns, so the prompt's cursor, the wrapping of the prompt, the
  `…` cut of long titles and the padding of lists and rules line up. A joined
  emoji (a ZWJ sequence or one with a skin tone) is measured by its parts, so
  it can still be off by a column or two.
- `/edit` opens the prompt in `$VISUAL`, else `$EDITOR`, else `vi`. Typing
  `/edit` replaces the prompt, so the editor opens on whatever follows the
  name: `/edit some draft` starts the editor on `some draft`. coax leaves the
  screen while the editor runs. The saved text, less its trailing newline,
  replaces the prompt with the cursor at the end, and is not sent. When the
  editor exits with an error the prompt stays as it was and a dim line says
  so. Ctrl+g opens the whole prompt in the same editor, as it stands.
- `/history` opens a box over the status line that lists the prompts sent in
  this run and the prompts of a resumed session, newest first. Each row is
  the prompt's first line cut to fit, with a dim `N lines` after it when the
  prompt has more. The box is at most 60 columns wide and shows 10 rows,
  scrolling past them. Up and Down (or `k` and `j`) and a click choose a
  row, Enter puts the prompt's full text in the prompt with the cursor at
  the end, and Esc closes the box. Up on an empty prompt opens the same box.
  With text in the prompt, Up only moves the cursor.
- Links in agent text, written `[text](target)` with an optional `"title"`.
  The scheme of the target decides what the link does.
  - `color:<name>` draws the text in the palette slot of that name, any of
    the terminal's or coax's own, without an underline. An unknown name is
    plain text.
    The link has no hover and no click.
  - `file:<path>` or `file:<path>#<line>` draws the text underlined in blue.
    Hovering opens a popup with the path relative to the folder coax runs in,
    the line, and the title. A click sends decade `show` with kind `file`, the
    absolute path and the line, to the pane named by `[links] decade_pane`
    (default `code`). With no decade connection the line under the prompt
    says so.
  - `http:` and `https:` links show the URL and title on hover. A click runs
    `xdg-open` only with `[links] open_urls = true`, which defaults to false.
  - Any other scheme named in `[links.schemes.<scheme>]` runs its `hover`
    command on hover, with `{target}` replaced by the part after the scheme,
    in coax's folder with a two second limit. The popup shows `loading…`,
    then the first 15 lines of output with escape sequences removed, or the
    error. Output is kept per target for the run. An optional `click` command
    runs detached on a click. A target that starts with `-` runs nothing.
  - A hovered link turns accent, and its popup closes when the pointer leaves.
    Links inside a table or code block that is wider than the screen are
    drawn but get no hover or click.
- A new-version notice. An agent entry may set `registry = "<id>"`, its id in
  the ACP registry. After the agent initializes, coax fetches that id's
  `agent.json` from the registry's repository with `curl` in the background.
  When the registry's `version` is later than the agent's reported version,
  the status line shows `<version> available`, dim, at the right. Only the
  numbers before any `-` or `+` are compared, so a preview suffix is ignored.
  The fetched version is cached for a day in `registry-<id>.json` under
  `$XDG_CACHE_HOME/coax` (`~/.cache/coax` without it). A failed fetch shows
  nothing and does not hold up the session. coax sends no information about
  you in the request.

### Changed

- An agent that exits no longer takes coax with it. The conversation stays on
  screen and scrolls and copies as before, with a notice `agent exited:` and
  the agent's status and last stderr lines. Sending is off, and the rule under
  the prompt says the agent has exited. ^c ^c and ^d ^d still quit. A call
  still running when the agent exits or a turn is cancelled shows `cancelled`
  in place of `running…`.
- A blank terminal no longer shows while the agent starts or a session loads.
  coax draws `starting <agent>…` or `loading session…`, and ^c quits from it.
- An agent that does not start names its command in the error. In the picker,
  its row says `failed:` with the reason and Enter on it does nothing, and a
  row whose session list failed says `sessions failed:`. A failure before the
  session opens no longer prints terminal reset codes.
- The scenario format has an `exit` step that ends the fake agent's process
  with a code and optional stderr text.
- A conversation that is taller than the pane at one width and shorter one
  column narrower no longer panics the scroll bar. The bar shows only when
  the narrower layout is still taller.
- A second permission request no longer cancels the first. The requests queue,
  the box shows the oldest with `N more` under it, and the next appears when
  one is answered. A box closes, as cancelled, when its turn ends.
- Ctrl+c while the agent works cancels the turn and also arms the exit guard,
  so a second Ctrl+c within 1.5 seconds quits when the agent ignores the
  cancel.
- The agent's answer to a setting change closes or marks only the menu that
  asked for it. A late answer used to close an unrelated menu or write its
  error into it.
- Picker boxes under the status line widen to fit their longest row, up to the
  screen width less four columns. They stopped at 60 columns and cut a model's
  description.
- Text from the agent, your earlier messages and thoughts now start a new
  entry when the message id changes, so two replayed messages in a row stay
  two bubbles. A message sent to a subagent shows under its call, dim and
  marked `>`, where it used to show as your own message.
- A subagent's call shows as `agent` from the start, and still does after a
  session is loaded. The Claude adapter keeps a subagent's history in its own
  transcript and replays it only to clients that speak the subagent sessions
  protocol, so a loaded session shows the call with no children, folded and
  with no marker.
- The token counts on the status line are session totals. Each turn's input,
  cached reads and writes add to the up count and its output to the down
  count. They used to show the last turn only.
- The `+N −M` size of an edit counts the lines that changed, taken from the
  same line diff the expanded view shows. It counted every line of the old and
  new texts. The permission box and a loaded session count the same way.
- The copy glyph is `⧉` (U+29C9) in place of a Nerd Font private-use
  character. Some fonts drew that wider than one column and pushed the box's
  right border out of line.
- Panning a code block or table counts display columns. A wide character cut
  by the window's edge shows as a space, where before the rows came out a
  column or more off.
- A long session no longer slows down. A transcript of 150 sections with code,
  a table and two command calls each cost 15 ms a frame, and every event or
  keystroke drew one. The transcript lays out only the entries on screen, keeps
  the text layouts for both widths it measures, and caches the rows under
  expanded edits and commands. A mouse move redraws only when the pointer
  enters or leaves something it highlights.
- An idle session no longer spins a core. A closed decade connection used to wake
  the run loop on every pass.
- The session has no panes and reads like a messaging thread. The
  conversation starts on the first row, and a dim status line under the
  prompt carries the settings, the plan summary, context, tokens, cost and
  decade when connected, and `idle` between turns. While a turn runs, a row
  above the prompt shows a spinner stepping through `◐◓◑◒`, `working`, the
  time since the prompt was sent and `esc to cancel`. While a permission
  request waits, the row reads `waiting for you` in yellow, with the time.
  Your messages sit in a rounded box
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
  row of key hints. When the agent has sent a plan, the status line shows how
  many steps are done, before the idle state.
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
- End jumps the conversation to the bottom only while the prompt is empty. With
  text in the prompt it moves the cursor to the end of the prompt.
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
