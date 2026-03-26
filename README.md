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

Agent harnesses are very useful, but they are designed to be launched from a specific working directory, like a codebase or a project folder, to perform a series of tasks. 

Sometimes we only want to use them for a quick operation, such as calling a remote service through MCP, or for a one-off task such as searching the web and writing a short report. In those cases, manually creating and cleaning up a temporary working directory is unnecessary friction.

`apg` (abbrv. for "agent playground") CLI solves this by letting you define a set of template working directories, i.e. playgrounds, and spin up temporary copies from them to launch an agent. When the work is done, the temporary directory is cleaned up automatically (unless you choose to keep it).

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
# you can also initialize a playground and include specific agent config templates
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
# run the default agent in an empty temporary playground
apg default
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

## Configuration layout

The CLI stores configuration under `~/.config/agent-playground`.

The actual layout on disk looks like this:

```text
~/.config/agent-playground/
├── config.toml
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

# Known agents. The key is the agent id used in `apg ... --agent <id>`,
# and the value is the shell command used to launch it.
[agent]
# Built-in default entries created by `apg init`.
claude = "claude"
opencode = "opencode"
# You can also define custom commands:
# opencode = "docker run --rm -it opencode/agent:latest"
# codex = "codex"

# Default runtime options inherited by every playground unless that
# playground overrides them in its own apg.toml.
[playground]
# Default agent id for playground runs.
# This value must exist in the [agent] table.
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
- `[agent].claude = "claude"`
- `[agent].opencode = "opencode"`
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
# This value must exist in the root config's [agent] table.
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
