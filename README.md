# Agent Playground

```
          _____                    _____                    _____          
         /\    \                  /\    \                  /\    \         
        /::\    \                /::\    \                /::\    \        
       /::::\    \              /::::\    \              /::::\    \       
      /::::::\    \            /::::::\    \            /::::::\    \      
     /:::/\:::\    \          /:::/\:::\    \          /:::/\:::\    \     
    /:::/__\:::\    \        /:::/__\:::\    \        /:::/  \:::\    \    
   /::::\   \:::\    \      /::::\   \:::\    \      /:::/    \:::\    \   
  /::::::\   \:::\    \    /::::::\   \:::\    \    /:::/    / \:::\    \  
 /:::/\:::\   \:::\    \  /:::/\:::\   \:::\____\  /:::/    /   \:::\ ___\ 
/:::/  \:::\   \:::\____\/:::/  \:::\   \:::|    |/:::/____/  ___\:::|    |
\::/    \:::\  /:::/    /\::/    \:::\  /:::|____|\:::\    \ /\  /:::|____|
 \/____/ \:::\/:::/    /  \/_____/\:::\/:::/    /  \:::\    /::\ \::/    / 
          \::::::/    /            \::::::/    /    \:::\   \:::\ \/____/  
           \::::/    /              \::::/    /      \:::\   \:::\____\    
           /:::/    /                \::/____/        \:::\  /:::/    /    
          /:::/    /                                   \:::\/:::/    /     
         /:::/    /                                     \::::::/    /      
        /:::/    /                                       \::::/    /       
        \::/    /                                         \::/____/        
         \/____/                                                           
```

`agent-playground` is a simple CLI for running agent in a temporary playground.
It is currently supported on macOS and Linux.

## Motivation

Agent harnesses are useful, but they are usually meant to run from a real working directory, such as a codebase or project folder.

Sometimes you only need an agent for a quick task, like calling a remote service through MCP or writing a short report from a web search. In those cases, creating and cleaning up a temporary working directory by hand is unnecessary friction.

`apg` ("agent playground") solves this by letting you define template working directories, called playgrounds, and launch agents in temporary copies of them. When the work is done, the temporary directory is cleaned up automatically unless you choose to keep it.

## Install

### With Cargo

```bash
cargo install agent-playground
```

`cargo install` on Windows is not supported.

### With installer script

```bash
curl https://github.com/observerw/agent-playground/releases/latest/download/install.sh | sh
```

## Usage

```bash
# initialize a playground in ~/.config/agent-playground/playgrounds
# choose a proper name for your playground, e.g. "notion" for notion MCP agent
# when git is available, this also initializes a git repository in the new playground
# `default` is reserved for the empty-playground subcommand and cannot be used as a playground id
apg init demo
# you can also initialize a playground and include agent config directories
# configured by [agent.<id>].config_dir and sourced from ~/.config/agent-playground/agents/<id>/
apg init demo --agent claude --agent codex --agent opencode

# list all configured playgrounds
apg list

# show detailed information for a playground
apg info demo

# print the absolute path to a playground template directory
apg path demo

# remove a playground from the config directory
apg remove demo
# skip the confirmation prompt
apg remove demo --yes

# run a playground with the default agent
# almost equal to `cd /some/temp/dir && claude`
apg demo
# run directly in a specific directory
# playground files are linked into this directory and removed on exit
apg demo ~/workspace/live-playground
# plain `apg` first uses configured `default_playground`
# and otherwise falls back to `apg default`
apg
# run the default agent in an empty temporary playground
apg default
# run the empty default playground directly in a specific directory
apg default ~/workspace/live-playground
# or specify the agent to run with
apg demo --agent codex
# or specify the agent for the empty playground
apg default --agent codex
# symlink-mount an external directory into the temporary playground
# you will see a `shared-context` directory in the playground
apg demo --with ~/workspace/shared-context
# or map it to a custom relative path inside the playground
# you will see a `context/shared` directory in the playground
apg default --with ~/workspace/shared-context:context/shared
# automatically save the temporary playground on normal exit (skips the interactive prompt)
apg demo --save
apg default --save
```

When the agent exits normally, `apg` asks whether to keep the temporary playground copy. Enter `y` to save it under the configured archive directory, or press Enter to discard it. Pass `--save` to skip the prompt and always save on normal exit.

When you pass `in_path` (`apg <playground_id> <in_path>` or `apg default <in_path>`), `apg` runs directly in that directory instead of a temp dir:

- If a playground source entry is a file and the destination name does not exist, `apg` creates a symlink.
- If a source entry is a directory and the destination name does not exist, `apg` creates a symlink to that directory.
- If both source and destination are directories, `apg` recursively applies the same rule to child entries.
- Any destination conflict is skipped. Existing content in `in_path` always wins.
- Exception: if a conflicting file is `AGENTS.md` or `CLAUDE.md` and the destination is an existing non-symlink text file, `apg` appends a managed block containing `@/absolute/path/to/playground-file` to the destination for the duration of the run.
- If `in_path` does not exist, `apg` creates it automatically.
- If `in_path` exists but is not a directory, `apg` exits with an error.
- On exit (including non-zero agent exit), `apg` removes links it created for this run and removes any managed `AGENTS.md` / `CLAUDE.md` include blocks it appended.
- In this mode, `--save` is ignored and no save prompt is shown.

Subcommands must come first. For example, `apg demo list` is invalid; use `apg list`.

## Shell completion

`apg` supports dynamic shell completion for playground ids via `clap_complete`.
Because the completion script calls back into `apg`, it stays in sync with the
playgrounds currently configured on your machine.

```sh
# zsh: enable for the current shell session
source <(COMPLETE=zsh apg)

# bash: enable for the current shell session
source <(COMPLETE=bash apg)

# fish: enable for the current shell session
COMPLETE=fish apg | source
```

## Configuration layout

The CLI stores configuration under `~/.config/agent-playground`.

The actual layout on disk looks like this:

```text
~/.config/agent-playground/
├── config.toml
├── agents/
│   ├── claude/
│   │   └── ...
│   └── opencode/
│       └── ...
├── playgrounds/
│   ├── demo/
│   │   ├── apg.toml
│   │   └── ...
│   └── another-playground/
│       ├── apg.toml
│       └── ...
└── saved-playgrounds/   # default archive directory; configurable
```

`config.toml` is the root config file. These are all supported fields:

```toml
# Directory used when you choose to keep a temporary playground after the
# agent exits.
# Relative paths are resolved against ~/.config/agent-playground.
saved_playgrounds_dir = "saved-playgrounds"

# Optional playground id used when running `apg` without an explicit
# playground argument.
# This value must refer to an existing configured playground.
default_playground = "demo"

# Known agents. The key is the agent id used in `apg ... --agent <id>`,
# and each [agent.<id>] can configure launch command and init copy destination.
[agent.claude]
# Command used to launch this agent. Defaults to "claude" when omitted.
cmd = "claude"
# Relative directory inside a playground that receives copied files from
# ~/.config/agent-playground/agents/claude/ when running `apg init ... --agent claude`.
# Defaults to ".claude/" when omitted.
# If the source directory does not exist, `apg init` still creates this target directory.
config_dir = ".claude/"

[agent.opencode]
cmd = "opencode"
config_dir = ".opencode/"

# You can also define custom agents:
# [agent.codex]
# cmd = "codex --fast"
# config_dir = ".codex/"

# Default runtime options inherited by every playground unless that
# playground overrides them in its own apg.toml.
[playground]
# Default agent id for playground runs.
# This value must exist as an [agent.<id>] entry in root config.
default_agent = "claude"
# Whether to load the playground template's `.env` into the agent process
# environment when running.
# When enabled, the `.env` file itself is still not copied into the
# temporary or saved playground.
load_env = false
# Strategy used to materialize playground files into the temporary directory.
# Accepted values: "copy" (default), "symlink", "hardlink".
create_mode = "copy"
```

Default values when `config.toml` is first created:

- `saved_playgrounds_dir = "saved-playgrounds"`
- `default_playground` is unset
- `[agent.claude].cmd = "claude"`
- `[agent.claude].config_dir = ".claude/"`
- `[agent.opencode].cmd = "opencode"`
- `[agent.opencode].config_dir = ".opencode/"`
- `[playground].default_agent = "claude"`
- `[playground].load_env = false`
- `[playground].create_mode = "copy"`

Each playground directory contains a flat `apg.toml` (not nested under
`[playground]`) which can override the inherited root defaults:

```toml
# Human-readable description shown in `apg list` and `apg info`.
description = "TODO: describe demo"

# Optional playground-specific default agent id.
# If omitted, the value from config.toml [playground].default_agent is used.
# This value must exist as an [agent.<id>] entry in root config.
default_agent = "codex"

# Optional playground-specific override for whether to load `.env`.
# If omitted, the value from config.toml [playground].load_env is used.
load_env = true

# Optional playground-specific strategy for materializing files into the
# temporary directory. Accepted values: "copy", "symlink", "hardlink".
# If omitted, the value from config.toml [playground].create_mode is used.
create_mode = "copy"
```

## License

MIT
