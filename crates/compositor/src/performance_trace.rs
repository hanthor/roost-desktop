//! Opt-in, bounded overview action-to-primary-scanout tracing for CI.
//!
//! No key symbols, client identities or text are recorded. The start timestamp
//! is compositor handling of an accepted overview action, not device arrival.
use std::time::Duration;

const LIMIT: u64 = 64;

/// Observations of the scene actually submitted, not cache population alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CardReadiness {
    pub expected: Option<usize>,
    pub rendered: usize,
    pub cached: usize,
    pub pending: usize,
}

impl CardReadiness {
    fn complete(self) -> bool {
        self.expected == Some(self.rendered)
    }
    fn json(self) -> String {
        format!(
            "{{\"expected\":{},\"rendered\":{},\"cache\":{},\"pending\":{}}}",
            self.expected
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".into()),
            self.rendered,
            self.cached,
            self.pending
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Input {
    id: u64,
    input_ns: u128,
    first_cards: Option<CardReadiness>,
    opening: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Presented {
    pub id: u64,
    pub input_ns: u128,
    pub queued_ns: u128,
    pub presented_ns: u128,
    pub sequence: u64,
    pub first_cards: CardReadiness,
    pub presented_cards: CardReadiness,
}

pub(crate) struct Trace {
    enabled: bool,
    count: u64,
    pending: Vec<Input>,
    in_flight: Option<(u128, Vec<Input>, CardReadiness)>,
}

impl Trace {
    pub fn from_env() -> Self {
        Self::new(std::env::var("TUNA_PERF_TRACE").as_deref() == Ok("1"))
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            count: 0,
            pending: Vec::new(),
            in_flight: None,
        }
    }

    pub fn input(&mut self, at: Duration, opening: bool) {
        if !self.enabled || self.count >= LIMIT {
            return;
        }
        // A reversal supersedes actions still waiting for their requested
        // scene. Already queued frames retain their own real flip association.
        let superseded: Vec<_> = self
            .pending
            .iter()
            .filter(|input| input.opening != opening)
            .copied()
            .collect();
        self.discard(&superseded);
        self.pending.retain(|input| input.opening == opening);
        self.count += 1;
        let input = Input {
            id: self.count,
            input_ns: at.as_nanos(),
            first_cards: None,
            opening,
        };
        self.pending.push(input);
        eprintln!(
            "tuna-perf-input: {{\"kind\":\"input\",\"id\":{},\"input_ns\":{},\"opening\":{},\"schema\":2}}",
            input.id, input.input_ns, input.opening
        );
    }

    /// Called only after the primary output's actual queue_buffer succeeds.
    pub fn queued(&mut self, at: Duration, cards: CardReadiness) {
        if !self.enabled || self.pending.is_empty() {
            return;
        }
        if self.in_flight.is_some() {
            // Never assign an event to an ambiguous/replaced pending frame.
            self.cancel();
            return;
        }
        for input in &mut self.pending {
            if input.first_cards.is_none() {
                input.first_cards = Some(cards);
                eprintln!("tuna-perf-input: {{\"kind\":\"card-candidate\",\"schema\":2,\"id\":{},\"candidate_ns\":{},\"preparation\":\"{}\",\"cards\":{}}}",
                    input.id, at.as_nanos(), if cards.complete() { "ready-on-first-candidate" } else { "incomplete-on-first-candidate" }, cards.json());
            }
        }
        if !cards.complete() {
            // Keep the original action timestamp. A changed dark background or
            // shell UI with missing cards is not a card-complete overview frame.
            return;
        }
        if cards.expected == Some(0) {
            // A close that supersedes an incomplete opening must never credit
            // that opening with a desktop frame containing no overview cards.
            let cancelled: Vec<_> = self
                .pending
                .iter()
                .filter(|input| input.opening)
                .copied()
                .collect();
            self.discard(&cancelled);
            self.pending.retain(|input| !input.opening);
        }
        if self.pending.is_empty() {
            return;
        }
        let inputs = std::mem::take(&mut self.pending);
        let queued_ns = at.as_nanos();
        for input in &inputs {
            eprintln!(
                "tuna-perf-input: {{\"kind\":\"queued\",\"id\":{},\"queued_ns\":{}}}",
                input.id, queued_ns
            );
        }
        self.in_flight = Some((queued_ns, inputs, cards));
    }

    /// A real primary page flip; absent kernel time cannot measure latency.
    pub fn presented(&mut self, at: Option<Duration>, sequence: u64) -> Vec<Presented> {
        let Some((queued_ns, inputs, cards)) = self.in_flight.take() else {
            return Vec::new();
        };
        let Some(at) = at else {
            self.discard(&inputs);
            return Vec::new();
        };
        let presented_ns = at.as_nanos();
        if inputs
            .iter()
            .any(|input| input.input_ns > queued_ns || input.input_ns > presented_ns)
        {
            self.discard(&inputs);
            return Vec::new();
        }
        inputs.into_iter().map(|input| {
            let row = Presented { id: input.id, input_ns: input.input_ns, queued_ns, presented_ns, sequence,
                first_cards: input.first_cards.expect("queued input observed its candidate"), presented_cards: cards };
            eprintln!("tuna-perf-input: {{\"kind\":\"presented\",\"id\":{},\"input_ns\":{},\"queued_ns\":{},\"presented_ns\":{},\"sequence\":{},\"schema\":2,\"cards_complete\":true,\"first_cards\":{},\"presented_cards\":{}}}", row.id, row.input_ns, row.queued_ns, row.presented_ns, row.sequence, row.first_cards.json(), row.presented_cards.json());
            row
        }).collect()
    }

    pub fn cancel(&mut self) {
        self.discard(&self.pending);
        self.pending.clear();
        if let Some((_, inputs, _)) = self.in_flight.take() {
            self.discard(&inputs);
        }
    }

    fn discard(&self, inputs: &[Input]) {
        for input in inputs {
            eprintln!(
                "tuna-perf-input: {{\"kind\":\"discarded\",\"id\":{}}}",
                input.id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ns(value: u64) -> Duration {
        Duration::from_nanos(value)
    }

    fn ready() -> CardReadiness {
        CardReadiness {
            expected: Some(2),
            rendered: 2,
            cached: 2,
            pending: 0,
        }
    }

    #[test]
    fn missing_cards_never_qualify_a_changed_or_presented_frame() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        let missing = CardReadiness {
            expected: Some(2),
            rendered: 1,
            cached: 4,
            pending: 0,
        };
        for tick in 20..100 {
            trace.queued(ns(tick), missing);
            assert!(trace.presented(Some(ns(tick + 1)), tick).is_empty());
        }
        trace.queued(
            ns(101),
            CardReadiness {
                expected: None,
                rendered: 0,
                ..missing
            },
        );
        assert!(trace.presented(Some(ns(102)), 102).is_empty());
        assert_eq!(trace.pending.len(), 1);
        assert_eq!(trace.pending[0].input_ns, 10);
    }

    #[test]
    fn a_cold_card_completion_keeps_original_input_and_first_candidate_receipt() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        let cold = CardReadiness {
            expected: Some(2),
            rendered: 0,
            cached: 0,
            pending: 2,
        };
        trace.queued(ns(20), cold);
        assert!(trace.presented(Some(ns(30)), 1).is_empty());
        trace.queued(ns(100), ready());
        let row = trace.presented(Some(ns(110)), 2).pop().unwrap();
        assert_eq!(row.input_ns, 10);
        assert_eq!(row.presented_ns - row.input_ns, 100);
        assert_eq!(row.first_cards, cold);
        assert_eq!(row.presented_cards, ready());
        assert_eq!(row.sequence, 2);
    }

    #[test]
    fn a_changed_layout_needs_its_actual_rendered_cards_not_old_cached_entries() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        let old = CardReadiness {
            expected: Some(2),
            rendered: 0,
            cached: 2,
            pending: 2,
        };
        trace.queued(ns(20), old);
        let changed = CardReadiness {
            expected: Some(3),
            rendered: 2,
            cached: 4,
            pending: 1,
        };
        trace.queued(ns(30), changed);
        assert!(trace.presented(Some(ns(40)), 1).is_empty());
        let complete = CardReadiness {
            rendered: 3,
            pending: 0,
            ..changed
        };
        trace.queued(ns(50), complete);
        let row = trace.presented(Some(ns(60)), 2).pop().unwrap();
        assert_eq!(row.first_cards.expected, Some(2));
        assert_eq!(row.presented_cards.expected, Some(3));
        assert_eq!(row.input_ns, 10);
    }

    #[test]
    fn closing_an_incomplete_opening_never_credits_it_as_complete() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        trace.queued(
            ns(20),
            CardReadiness {
                rendered: 0,
                ..ready()
            },
        );
        trace.input(ns(30), false);
        trace.queued(
            ns(40),
            CardReadiness {
                expected: Some(0),
                rendered: 0,
                cached: 0,
                pending: 2,
            },
        );
        let rows = trace.presented(Some(ns(50)), 3);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 2);
        assert_eq!(rows[0].input_ns, 30);
    }

    #[test]
    fn a_pending_old_frame_cannot_complete_a_new_input() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        assert!(trace.presented(Some(ns(20)), 1).is_empty());
        trace.queued(ns(30), ready());
        assert_eq!(
            trace.presented(Some(ns(40)), 2),
            vec![Presented {
                id: 1,
                input_ns: 10,
                queued_ns: 30,
                presented_ns: 40,
                sequence: 2,
                first_cards: ready(),
                presented_cards: ready(),
            }]
        );
        assert!(trace.presented(Some(ns(50)), 3).is_empty());
    }

    #[test]
    fn lock_reset_and_unknown_or_stale_kernel_times_never_report_latency() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        trace.queued(ns(20), ready());
        trace.cancel();
        assert!(trace.presented(Some(ns(30)), 1).is_empty());
        trace.input(ns(40), true);
        trace.queued(ns(50), ready());
        assert!(trace.presented(None, 2).is_empty());
        trace.input(ns(60), true);
        trace.queued(ns(70), ready());
        assert!(trace.presented(Some(ns(55)), 3).is_empty());
    }

    #[test]
    fn kernel_vblank_can_precede_the_userspace_queue_completion_stamp() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        trace.queued(ns(40), ready());
        let rows = trace.presented(Some(ns(30)), 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].presented_ns - rows[0].input_ns, 20);
    }

    #[test]
    fn coalesced_inputs_share_only_the_frame_that_was_actually_queued() {
        let mut trace = Trace::new(true);
        trace.input(ns(10), true);
        trace.input(ns(15), true);
        trace.queued(ns(20), ready());
        trace.input(ns(25), true);
        let rows = trace.presented(Some(ns(30)), 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [1, 2]);
        trace.queued(ns(40), ready());
        assert_eq!(trace.presented(Some(ns(50)), 2)[0].id, 3);
    }

    #[test]
    fn disabled_trace_and_fixed_lifetime_limit_bound_collection() {
        let mut trace = Trace::new(false);
        trace.input(ns(1), true);
        trace.queued(ns(2), ready());
        assert!(trace.presented(Some(ns(3)), 1).is_empty());
        let mut trace = Trace::new(true);
        for _ in 0..100 {
            trace.input(ns(1), true);
        }
        assert_eq!(trace.count, LIMIT);
        assert_eq!(trace.pending.len(), LIMIT as usize);
    }
}
