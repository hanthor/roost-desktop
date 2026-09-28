# Rust Wayland Desktop

A new, independent Wayland desktop session with a GNOME-inspired everyday workflow. This project is not a GNOME Shell rewrite and does not promise compatibility with GNOME Shell extensions or private Mutter APIs.

The compositor is a long-lived Rust process built with Smithay. The shell UI runs as a supervised Wayland client. The first delivery target is a nested developer preview that can map ordinary applications and recover its shell UI after a shell crash.

## Project status

Design and planning only; no compositor implementation has started. The planning framework covers baseline research, the nested recovery slice, shell parity, hardware/app compatibility, secure system integration, optional extensions, and release readiness.

## Planning

- [Program architecture](docs/architecture.md)
- [Program roadmap and requirement traceability](docs/roadmap.md)
- [Verification and test strategy](docs/test-strategy.md)
- [Research intake rules](docs/research/README.md)
- [First spek: nested compositor and shell recovery](.spektacular/specs/20260927170317-a01f0011-001-nested-compositor-shell-recovery.md)
- [Architecture decisions](docs/adr/README.md)

Spektacular is initialized for Codex. Specs and plans are managed through its CLI; each program unit has a spek plus draft plan, context, and research artifacts. Draft plans must be reviewed against current implementation and open decision gates before their implementation workflow starts.

## Continue with Spektacular

Install the CLI if needed (`go install github.com/hivecommons/spektacular@latest`), then ensure `$(go env GOPATH)/bin` is on `PATH`.

Use the spek's full timestamp-prefixed name from `spektacular spec file list`:

```sh
spektacular version check
spektacular plan new --data '{"name":"20260927170317-a01f0011-001-nested-compositor-shell-recovery"}'
```

The plan workflow should refresh its draft from the current source and the linked spek. Complete its walkthrough and review before starting implementation. The implementation workflow starts with the corresponding full plan name after approval. See [Spektacular](https://github.com/hivecommons/spektacular) for the current workflow and CLI details.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
