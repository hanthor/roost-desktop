# Development guide

## Prerequisites

Before you begin, ensure you have the following installed:

- **Rust** (1.70 or later) — get it from [rustup.rs](https://rustup.rs)
- **Go** (1.21 or later) — used by Spektacular; get it from [go.dev/dl](https://go.dev/dl)
- **pkg-config** — required for wayland/libadwaita dependency resolution

### Install Spektacular CLI

Spektacular manages this project's specs, plans, and design documents. Install the CLI:

```sh
go install github.com/hivecommons/spektacular@latest
```

Then add the Go binary directory to your `PATH`:

```sh
export PATH="$(go env GOPATH)/bin:$PATH"
```

Verify the installation:

```sh
spektacular version check
```

## Project structure

This is a Spektacular project. Code and planning artifacts are in separate locations:

| Directory | Purpose |
|-----------|---------|
| `crates/` | Rust source code for all deliverables |
| `crates/compositor` | Main compositor built with Smithay; handles seats, outputs, focus, input |
| `crates/shell-host` | Shell UI (panel, overview, notifications) running as a supervised Wayland client |
| `crates/shell-control-schema` | Versioned protocol schema for compositor↔shell communication |
| `crates/greeter` | Login greeter UI (recovery/lock flow) |
| `.spektacular/` | Spektacular specs, plans, and context documents (managed by `spektacular` CLI only) |
| `docs/` | Developer-facing architecture, decisions, roadmap, and verification strategy |
| `docs/adr/` | Architecture Decision Records |
| `scripts/` | Automation and developer tools |

**Important:** Never modify files under `.spektacular/` directly. Use `spektacular spec`, `spektacular plan`, and other CLI commands to manage them. See [AGENTS.md](../AGENTS.md) for details.

## First-time setup

1. Clone the repository:
   ```sh
   git clone https://github.com/hanthor/rust-wayland-desktop.git
   cd rust-wayland-desktop
   ```

2. Verify prerequisites are installed:
   ```sh
   rustc --version
   cargo --version
   go version
   spektacular version check
   ```

3. Build the compositor and shell:
   ```sh
   cargo build --workspace
   ```

4. (Optional) Build with release optimizations:
   ```sh
   cargo build --workspace --release
   ```

## Trying the nested developer preview

The 001 nested-compositor-shell-recovery slice is the first buildable vertical. Launch it locally:

```sh
scripts/rwd-nested run
```

This starts a nested Wayland session with the compositor and shell panel. A nested window appears; inside it, you can run Wayland clients. See [nested-session.md](nested-session.md) for crash journeys, environment details, and failure diagnostics.

## Understanding the delivery roadmap

This project uses a staged planning framework:

- **Spek** (specification): an isolated delivery unit with requirements, dependencies, and acceptance criteria
- **Plan** (implementation plan): the concrete steps and test strategy for a spek
- **Context and research**: supporting analysis and decision justification

View the program speks and their status:

```sh
spektacular spec file list
```

See the complete roadmap, dependency graph, and requirements traceability in [docs/roadmap.md](roadmap.md).

## Contributing

1. **Read the architecture** — [docs/architecture.md](architecture.md) explains process boundaries, authority, and release tiers
2. **Understand the roadmap** — [docs/roadmap.md](roadmap.md) shows where your work fits
3. **Check test strategy** — [docs/test-strategy.md](test-strategy.md) outlines verification requirements before your code merges
4. **Review ADRs** — [docs/adr/](adr/) documents important architectural decisions

For code contributions, ensure:
- Unit tests pass: `cargo test --workspace`
- Code follows Rust conventions and the project's decision records
- Your changes align with an approved spek and plan (file an issue to propose a new one)

## Debugging and logs

The nested session records diagnostics to `$XDG_STATE_HOME/rwd-nested/<socket>/nested.log` (or `~/.local/state/rwd-nested/<socket>/nested.log`). This includes:

- Compositor lifecycle events
- Shell supervision/restart attempts
- Control protocol errors (scrubbed of sensitive data)
- Frame timing and focus changes

Window titles, activation tokens, and frame content are never logged. See [nested-session.md](nested-session.md#failure-artifacts-and-redaction) for the redaction policy.

## Common tasks

### Run tests
```sh
cargo test --workspace
```

### Check code formatting
```sh
cargo fmt --check --all
```

### Run clippy linter
```sh
cargo clippy --workspace --all-targets
```

### Build documentation
```sh
cargo doc --workspace --open
```

### View Spektacular plan for the current slice
```sh
spektacular plan file show 20260927170317-a01f0011-001-nested-compositor-shell-recovery
```

## Getting help

- Architecture decisions: [docs/adr/](adr/)
- System design: [docs/architecture.md](architecture.md)
- Nested session mechanics: [docs/nested-session.md](nested-session.md)
- Verification strategy: [docs/test-strategy.md](test-strategy.md)
- Spektacular workflow: [AGENTS.md](../AGENTS.md) or `spektacular help`
- GNOME baseline and compatibility: [docs/research/](research/)
