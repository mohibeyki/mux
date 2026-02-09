# Mux

**Mux** is an interactive TUI that runs multiple CLI commands in parallel and merges their output into a single stream.

> **Note:** This project is under active development.

## Features

- **Parallel execution** — run commands concurrently with expansion syntax (e.g., `[n=1-64] cmd {n}`)
- **Shell history suggestions** — fuzzy search across Bash, Zsh, and Fish history with frequency ranking
- **Argument-aware suggestions** — context-aware completions for commands, arguments, and values
- **Inline preview** — ghost text suggestions with word-by-word acceptance
- **PTY-based execution** — full terminal emulation with ANSI color passthrough
- **Structured logging** — glog-style logs with rotation in `$XDG_STATE_HOME/mux/logs/`

## Quick Start

```bash
cargo build --release
./target/release/mux
```

On first run, Mux will create a database at `$XDG_STATE_HOME/mux/history.db` and sync your shell history.

## Usage

```bash
# Rebuild the history index
mux --rebuild

# Run with debug logging
RUST_LOG=debug mux
```

## Development

```bash
cargo test
cargo build --release
RUST_LOG=debug cargo run
```

## License

GPL-3.0 — see [LICENSE](LICENSE) for details.
