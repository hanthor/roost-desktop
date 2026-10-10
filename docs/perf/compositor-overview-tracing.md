# Tuna Desktop overview action-to-scanout tracing

For an explicit performance fixture, set `TUNA_PERF_TRACE=1` before starting `tuna-compositor --backend drm`. Normal sessions leave tracing disabled. The CI-only greetd VM wrapper enables it and forwards stderr through its existing journal/serial route. No key symbols, text, window titles or client IDs enter these records.

An accepted overview toggle records an anonymous ID and CLOCK_MONOTONIC handling timestamp. That action is associated with the first successfully queued primary-output scene whose expected workspace wallpaper cards are all renderable. A changed shell or dark overview background while cards are pending does not complete the action; its original handling timestamp stays pending. A frame already pending before the action cannot complete it. The corresponding actual primary kernel page flip records its timestamp and sequence; a missing or stale kernel timestamp produces a discarded record rather than a latency sample. Lock, inactive-seat and ambiguous replaced-frame paths discard pending associations. At most 64 actions are recorded during a compositor’s lifetime.

Records in stderr/serial have the prefix `tuna-perf-input: ` and JSON `kind` values `input`, `card-candidate`, `queued`, `presented` or `discarded`. Schema 2 input rows also identify opening versus closing. The first `card-candidate` records expected/rendered/cache/pending card counts and whether cards were `ready-on-first-candidate` or `incomplete-on-first-candidate`. These labels describe the submitted candidate, not proof that preparation completed before input. A schema 2 presented row contains `id`, `input_ns`, `queued_ns`, `presented_ns`, the actual primary page-flip `sequence`, `cards_complete: true`, and both `first_cards` and `presented_cards` counts. Cache counts alone never establish readiness: rendered elements must match the current layout's expected count. A close superseding an incomplete opening discards that opening, rather than crediting it with a desktop frame containing zero cards. Subtract `input_ns` from `presented_ns` for the handling-to-scanout interval. Check the input/queue records and require `input_ns <= queued_ns` and `input_ns <= presented_ns` before using it. The kernel reports a vblank timestamp, which can precede userspace’s post-submission `queued_ns` marker; that ordering alone does not identify an old frame. Multiple coalesced actions can share the same presented frame and are explicitly separate input IDs.

This measures the compositor’s accepted overview action through its resulting primary scanout. It excludes physical-device transport before handling, display scanout position, the GTK shell’s asynchronous changes, and animation completion. It is not a general application input latency measure. GNOME tracing and a fresh paired candidate run are still needed for #73; no comparative latency result is asserted here.

## Paired benchmark scope

The existing QMP benchmark observes a broad pixel change. Its
`overview_first_response_upper_s` field (and legacy
`overview_observed_upper_s` alias) remains a first-response measurement. It can
see a changed shell or dark background before asynchronous cards are ready and
cannot qualify completed overview latency. Reports explicitly record
`overview_card_complete_scanout.status: unavailable` until native readiness and
associated presentation receipts are actually acquired. No receipt is inferred
from a fast response, an apparently prepared screenshot, or cache size.

The trace's card-complete scanout interval is separate from QMP first response
and from animation completion. It does not prove all GTK overview content has
finished rendering. Cold and prepared candidate cases must be retained
separately; changing layout requires that layout's actual rendered cards. The
paired lane's existing whole-run CPU, RSS and PSS sampling includes startup and
preparation without adding a hidden wait or omitting prewarm work. GPU work and
total power still need their own qualified evidence.
