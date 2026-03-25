# mailman

`mailman` is a Rust terminal app scaffold for Gmail. The current project gives you:

- a CLI with basic commands
- config bootstrapping in a predictable app directory
- browser-based OAuth sign-in with local token storage
- a paginated inbox TUI with local filter/search/grouping and full-message viewing

## Planned tasks

The scaffold includes commands for:

- default launch to open the inbox terminal UI
- `init` to create a config file
- `auth` to complete the OAuth flow and store tokens
- `inbox` to list recent messages
- `read` to inspect a message
- `send` to send a message

## Quick start

```bash
cargo run -- init
```

That creates a config file under your OS app config directory, for example:

- Linux: `~/.config/mailman/config.toml`
- macOS: `~/Library/Application Support/mailman/config.toml`
- Windows: `%APPDATA%\\mailman\\config.toml`

## Project layout

```text
src/
  app.rs          command dispatch
  cli.rs          clap command definitions
  config.rs       config and filesystem bootstrap
  gmail/
    client.rs     Gmail API boundary
    models.rs     domain models used by the CLI
    mod.rs
  main.rs
```

## Current status

Implemented:

1. Google OAuth desktop sign-in
2. Inbox listing
3. Default TUI launch with page navigation
4. Full message fetch for the selected mail
5. Local filter, search, and grouping in the inbox TUI

Remaining major task:

1. Implement Gmail send support
