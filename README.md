# agent-playground

`agent-playground` is a small CLI for running agent in a temporary playground.

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