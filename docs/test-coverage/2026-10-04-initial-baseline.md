# Initial coverage baseline, before the session-lock wire cases

[Instrumented run 37188464108](https://github.com/tuna-os/tuna-desktop/actions/runs/37188464108) passed on the exact source below, using Rust1.94.1 and cargo-llvm-cov0.9.1. The `measured-test-coverage` artifact contains the original LLVM JSON, test log, environment and reproduction command. The JSON SHA256 is `3a724447eb3fa60d4370fb24de7ad1f85c96785683ca9dc3c6c020c75826add9`.

This dated baseline predates the four new session-lock wire cases and percentage gates. The measured low session-lock result deliberately does not satisfy the subsequent60% floor. Source-file totals include inline unit-test code; the coverage scope and its limitations remain explicit.

# Measured workspace test coverage

Source revision: `849466b73a3a2a6e65fbd88772f4c1b6fe3a49de`

LLVM coverage from instrumented workspace tests. Integration-test files and external dependencies are excluded from these source-file aggregates; inline unit-test code remains included where it shares a source file. GUI journeys, hardware paths and doctests are not included. Stable-toolchain line and region coverage does not measure branch coverage.

| Crate | Lines | Regions |
|---|---:|---:|
| roost-compositor | 8634/14418 (59.88%) | 14044/22574 (62.21%) |
| roost-greeter | 357/430 (83.02%) | 596/744 (80.11%) |
| roost-shell-control | 352/405 (86.91%) | 489/553 (88.43%) |
| roost-shell-gtk | 3562/14233 (25.03%) | 5882/25481 (23.08%) |
| roost-shell-host | 12664/15156 (83.56%) | 22332/26075 (85.65%) |
| roost-wallpaper | 278/294 (94.56%) | 559/613 (91.19%) |

## Security modules

| Module | Lines | Regions |
|---|---:|---:|
| `crates/compositor/src/lock.rs` | 135/142 (95.07%) | 245/252 (97.22%) |
| `crates/compositor/src/unlock.rs` | 147/187 (78.61%) | 209/267 (78.28%) |
| `crates/compositor/src/session_lock.rs` | 20/81 (24.69%) | 17/107 (15.89%) |
| `crates/compositor/src/control.rs` | 568/852 (66.67%) | 754/1095 (68.86%) |
| `crates/shell-control-schema/src/lib.rs` | 352/405 (86.91%) | 489/553 (88.43%) |

This first measurement establishes the baseline for reviewed module-specific floors. Missing crate/module data fails the job; percentage thresholds, complete boundary cases and artifact enforcement remain tracked by #8. No security certification is inferred from a coverage percentage.
