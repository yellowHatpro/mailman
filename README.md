# mailman

`mailman` is a Rust terminal app scaffold for Gmail. The current project gives you:

- a CLI with basic commands
- config bootstrapping in a predictable app directory
- browser-based OAuth sign-in with local token storage
- a Gmail client boundary ready for Gmail API wiring

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

## Next implementation step

Wire the Gmail client to the real Gmail API:

1. Create a Google Cloud project.
2. Enable the Gmail API.
3. Create OAuth client credentials.
4. Store client metadata in the config file.
5. Use `cargo run -- auth` to sign in and save tokens.
6. Implement Gmail API requests for inbox/read/send in `src/gmail/client.rs`.
