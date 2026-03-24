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
          /:::/    /                  ~~               \:::\/:::/    /     
         /:::/    /                                     \::::::/    /      
        /:::/    /                                       \::::/    /       
        \::/    /                                         \::/____/        
         \/____/                                                           
```

`agent-playground` is a simple CLI for running agent in a temporary playground.

## Motivation

Agent harnesses are very useful, but they are designed to be launched from a specific working directory, like a codebase or a project folder, to perform a series of tasks. 

Sometimes we only want to use them for a quick operation, such as calling a remote service through MCP, or for a one-off task such as searching the web and writing a short report. In those cases, manually creating and cleaning up a temporary working directory is unnecessary friction.

`apg` (abbrv. for "agent playground") CLI solves this by letting you define a set of template working directories, i.e. playgrounds, and spin up temporary copies from them to launch an agent. When the work is done, the temporary directory is cleaned up automatically (unless you choose to keep it).

## Install

### With Cargo

```bash
cargo install agent-playground
```

### With installer script

```bash
curl https://github.com/observerw/agent-playground/releases/latest/download/install.sh | sh
```

## Usage

```bash
# initialize a playground in ~/.config/agent-playground/playgrounds
# choose a proper name for your playground, e.g. "notion" for notion MCP agent
apg init demo
# you can also initialize a playground and include specific agent config templates
apg init demo --agent claude --agent codex

# list all configured playgrounds
apg list

# run a playground with the default agent
# almost equal to `cd /some/temp/dir && claude`
apg demo
# or specify the agent to run with
apg demo --agent codex
```

When the agent exits, `apg` asks whether to keep the temporary playground copy. Enter `y` to save it under the configured archive directory, or press Enter to discard it.

## Configuration layout

The CLI stores configuration under `~/.config/agent-playground`.

`config.toml` defines the known agents and default selection:

```toml
default_agent = "claude"
saved_playgrounds_dir = "~/Download/saved-playgrounds"

[agent]
claude = "claude"
opencode = "opencode"
# or you can specify a custom command:
# opencode = "docker run --rm -it opencode/agent:latest"
```

Each playground gets its own `apg.toml`:

```toml
# description of the playground, shown in `apg list` output.
description = "TODO: describe demo"

# whether to load the playground template's `.env` file into the agent process environment before launch.
load_env = false
```

JSON Schema for these files can be generated directly from `agent_playground::config::RootConfigFile::json_schema()` and `agent_playground::config::PlaygroundConfigFile::json_schema()`.

## License

MIT
