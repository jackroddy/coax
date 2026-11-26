# coax

> **<u>co</u>nversational <u>a</u>gent e<u>x</u>change**

## about

`coax` is a simple agentic coding TUI that works with any agent that implements
[ACP](https://agentclientprotocol.com). After using the Claude Code CLI for a
while, I found that two things really bothered me:

1. the agent chat fills with message spam, causing pointless/distracting visual clutter
2. there are very few options for letting agents visually format their messages
   (for some reason, Claude Code even strips out ANSI color codes from agent messages)

I've only tested `coax` with Claude Code, using the
[Claude ACP adapter](https://github.com/agentclientprotocol/claude-agent-acp).

## signing in

`coax` does not sign in to any agent platforms. If you need to log in to one,
sign in with the agent's own tooling before you start `coax`.

## installation

    cargo install coax

## configuration

For configuration, coax reads `~/.config/coax/config.toml`:

```toml
# set your default agent
default = "claude"

# define the set of agents that
# coax offers for a new session
[agents.claude]
command = "claude-agent-acp"
env = { CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION = "false" }
registry = "claude-acp"

[agents.bob]
command = "bob"

[links]
open_urls = true

[links.schemes.man]
hover = ["whatis", "{target}"]
click = ["foot", "man", "{target}"]

[palette]
accent = "#DA745B"
muted = "#6D6977"
selection = "#37344A"
foreground = "#E7DDC8"
red = "#C26A7A"
yellow = "#DA915C"
orange = "#DA745B"
green = "#69A676"
cyan = "#57ABAD"
blue = "#5C9FB9"
magenta = "#A26DC9"
```

## features

- a session picker opens by default on a bare `coax` call
- each message type is visually distinct
- agent tool call messages stay compact unless you want to inspect them
- the session context is always on the screen
- agents can format messages in markdown
- you can mark parts of an agent message to easily reference them in a follow-up prompt

![a session picked in the picker, a run of tool calls folded and opened, a markdown reply panned, copied and switched to raw, and two passages of a reply marked and answered](assets/tour.gif)

## license

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
