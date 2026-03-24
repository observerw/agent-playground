# agent-playground

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

`agent-playground` is a CLI for running agent in a temporary playground.

## Motivation

Agent harnesses are already very useful, but they usually need to be launched from a specific working directory.

Sometimes we only want to use them for a quick operation, such as calling a remote service through MCP like Notion, or for a one-off task such as searching the web and writing a short report. In those cases, manually creating a fresh working directory first is unnecessary friction.

`agent-playground` solves this by letting you define a set of template working directories, called playgrounds, and spin up temporary copies from them to launch an agent. When the work is done, the temporary directory is cleaned up automatically (unless you choose to keep it).

## Install

### With Cargo

```bash
cargo install agent-playground
```

### With installer script

```bash
curl https://github.com/observerw/agent-playground/releases/latest/download/install.sh | sh
```

The installer supports:

- `APG_INSTALL_DIR=/custom/bin` to choose the install directory
- `APG_VERSION=0.1.0` to pin a specific release
- `APG_REPO=<owner>/<repo>` if you run the unpatched template directly

## Release

This repository is configured to publish to crates.io from GitHub Actions via trusted publishing.

On crates.io, open `agent-playground` and go to `Settings -> Trusted Publishing`, then add this GitHub configuration:

- Repository owner: `observerw`
- Repository name: `agent-playground`
- Workflow filename: `release.yml`
- Environment: `release`

The release workflow publishes when you push a tag like `v0.1.0`, and it uses `rust-lang/crates-io-auth-action@v1` to exchange the GitHub Actions OIDC token for a short-lived crates.io publish token.

## Usage

Initialize a playground:

```bash
apg init demo
```

List all playgrounds:

```bash
apg list
```

Run a playground with the default agent:

```bash
apg demo
```

Run a playground with a specific agent:

```bash
apg demo --agent opencode
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
description = "TODO: describe demo"
```
