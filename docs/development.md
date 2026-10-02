# Development Guide

Welcome to Rust Wayland Desktop (RWD). This guide covers everything you need to set up your development environment, understand the codebase structure, and contribute to the project.

## Prerequisites

Before you start, ensure you have the following installed:

### Core toolchain
- **Rust** (latest stable or 1.82+): Install from [rustup.rs](https://rustup.rs)
- **Cargo**: Installed automatically with Rust
- **Git**: For version control

### System dependencies
On Ubuntu/Debian:
```bash
sudo apt-get install -y \
  libwayland-dev \
  libxkbcommon-dev \
  libpipewire-0.3-dev \
  libgtk-4-dev \
  libadwaita-1-dev \
  libsystemd-dev \
  pkg-config
```

On Fedora:
```bash
sudo dnf install -y \
  wayland-devel \
  libxkbcommon-devel \
  pipewire-devel \
  gtk4-devel \
  libadwaita-devel \
  systemd-devel \
  pkg-config
```

### Spektacular CLI
RWD uses [Spektacular](https://github.com/projectbluefin/spektacular) for specification and planning workflows. Install the CLI:

```bash
# Clone the Spektacular repository
git clone https://github.com/projectbluefin/spektacular.git
cd spektacular
cargo install --path .
```

Verify installation:
```bash
spektacular version check
```

## Project Structure

The project is organized as a Cargo workspace with four main crates:

```
rust-wayland-desktop/
├── crates/
│   ├── compositor/          # rwd-compositor binary
│   │                        # Smithay-based Wayland compositor core
│   │                        # Handles rendering, input, protocol negotiation
│   │
│   ├── shell-host/          # rwd-shell-host binary
│   │                        # Shell UI client that connects to compositor
│   │                        # Responsible for panels, widgets, window chrome
│   │
│   ├── shell-control-schema/# Shared protocol and serialization
│   │                        # Messages between compositor and shell-host
│   │                        # Uses postcard for compact encoding
│   │
│   └── greeter/             # rwd-greeter binary
│                            # Login/authentication UI
│
├── docs/
│   ├── architecture.md      # Program architecture and design principles
│   ├── test-strategy.md     # Verification layers and test environments
│   ├── roadmap.md           # Feature roadmap and requirement traceability
│   ├── adr/                 # Architecture Decision Records
│   └── nested-session.md    # Nested session architecture and crash recovery
│
├── .spektacular/            # Spektacular specifications and planning
│   ├── specs/               # Spec documents for each work slice
│   ├── plans/               # Implementation plans tracked by spec
│   ├── knowledge/           # Research and design notes
│   └── work/                # Tracking data for each plan state
│
├── scripts/
│   ├── roost-nested         # One-command nested session launcher and journey harness
│   ├── roost-journey        # App mapping and interaction test harness
│   ├── roost-app-content    # Content generation for testing
│   └── roost-scroll-proof   # Scroll mode correctness proof
│
└── Cargo.toml              # Workspace configuration and pinned dependencies
```

## Setting Up Your Environment

1. **Clone the repository:**
   ```bash
   git clone https://github.com/hanthor/rust-wayland-desktop.git
   cd rust-wayland-desktop
   ```

2. **Build all crates:**
   ```bash
   cargo build
   ```

3. **Run tests (current: planning phase, no runtime tests yet):**
   ```bash
   cargo test
   ```

## Running the Nested Compositor

The nested compositor runs inside your current Wayland or X11 session for safe development and testing. It creates a virtual RWD session as a client of your host display.

### First-time setup

```bash
# Build and start a nested RWD session
./scripts/roost-nested run
```

This command:
- Builds both `rwd-compositor` and `rwd-shell-host` in release mode
- Starts the compositor on a private socket (named `roost-nested-<pid>` by default)
- Logs output to `~/.local/state/roost-nested/roost-nested-<pid>/nested.log`
- Displays the socket name and log path for reference

The session runs until you press Ctrl+C or terminate the process.

### Testing shell crashes and recovery

RWD implements a supervised shell-host restart mechanism. Test it with:

```bash
# In one terminal, start the nested session:
./scripts/roost-nested run

# In another terminal, simulate a shell crash:
./scripts/roost-nested kill-shell --socket roost-nested-<pid>
```

The compositor will:
1. Detect the shell-host crash
2. Display a recovery overlay
3. Restart the shell-host automatically within a bounded restart budget
4. Keep all application connections alive during the restart

See `docs/nested-session.md` for details on the crash recovery design.

## Understanding Spektacular Workflows

RWD uses Spektacular for planning and tracking implementation work. Each work item has:

- **Spec**: A specification document (in `.spektacular/specs/`)
- **Plan**: Implementation roadmap and checklist (in `.spektacular/plans/`)
- **Context**: Mutable tracking state and work artifacts

### Common Spektacular commands

```bash
# List all specs and their plan status
spektacular spec file list

# View a specific spec
spektacular spec show <spec-id>

# Create a new spec (typically for maintainers)
spektacular spec new --title "My feature" --category implementation

# Update plan state (e.g., transitioning from draft to ready)
spektacular plan <spec-id> --state ready
```

For more details, see the [Spektacular documentation](https://github.com/projectbluefin/spektacular).

## Architecture and Design

For deeper understanding of RWD's design:

- **Architecture overview**: Read `docs/architecture.md` for the compositor/shell separation, nested recovery design, and long-term roadmap
- **Design decisions**: Check `docs/adr/` (Architecture Decision Records) for rationale on key choices (e.g., Smithay pinning, backend selection)
- **Verification strategy**: See `docs/test-strategy.md` for how we validate each layer (unit tests, protocol probes, integration tests, hardware qualification)
- **Test strategy and matrix**: RWD targets multiple environments, GPU configurations, and client types; the test strategy document details the coverage plan

## Workflow for contributors

1. **Choose a spec**: Find an open spec in `.spektacular/specs/` or create one
2. **Review the plan**: Check the associated plan in `.spektacular/plans/` for implementation guidance
3. **Implement**: Edit crates as needed, using the test strategy guide to add tests
4. **Test locally**: Run `cargo build`, `cargo test`, and use `./scripts/roost-nested run` to validate
5. **Create a PR**: Reference the spec number and plan state in your PR body
6. **Update spec/plan state**: Once merged, update the spec and plan in Spektacular to reflect completion

## Common tasks

### Adding a new protocol feature
1. Define the protocol in `crates/shell-control-schema/`
2. Implement compositor side in `crates/compositor/`
3. Implement shell-host side in `crates/shell-host/`
4. Add a protocol probe in `.spektacular/work/` if appropriate
5. Document the change in `docs/adr/` if it's a significant choice

### Writing a test
- Unit tests: Add inline tests in the crate (follow `#[cfg(test)]` patterns)
- Protocol probes: Add scripts under `scripts/`
- Nested integration: Use `./scripts/roost-journey` as a base or add a new harness

**Timing rule (#72).** Never gate a test on a fixed iteration budget or a
sleep length: a loaded CI runner exhausts both. Wait on a wall-clock
deadline that is generous (seconds, not milliseconds) and only makes a
slow run slower, or make the ordering deterministic (a latch the test
releases, a manual clock, a counter of finished workers). Assert on
state, never on elapsed time, except for explicit no-hang bounds. There
is no retry policy for deterministic tests: `gh run rerun --failed` is a
diagnostic, not a fix.

### Debugging
- Compositor logs: Check the output of `./scripts/roost-nested run` or tail the log file
- Shell-host logs: Printed to the same log file as the compositor
- Nested shell interaction: Use `./scripts/roost-nested shell-pid` to identify the shell process for attaching a debugger

## Reporting issues

If you find a bug or have a feature request:

1. Check existing issues in the repository
2. Open a new issue with:
   - Clear title and description
   - Steps to reproduce (for bugs)
   - Expected vs. actual behavior
   - Environment (distro, GPU, Rust version, RWD commit)
3. Reference relevant specs in `.spektacular/specs/` if applicable

## Further reading

- **Nested session architecture**: `docs/nested-session.md`
- **Roadmap**: `docs/roadmap.md` for the full vision and current priorities
- **Smithay documentation**: https://docs.rs/smithay/latest/smithay/
- **Wayland protocol specs**: https://wayland.freedesktop.org/
- **Spektacular**: https://github.com/projectbluefin/spektacular

Welcome to the project, and happy hacking! 🚀
