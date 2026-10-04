//! Opt-in, bounded overview action-to-primary-scanout tracing for CI.
//!
//! No key symbols, client identities or text are recorded. The start timestamp
//! is compositor handling of an accepted overview action, not device arrival.
use std::time::Duration;

const LIMIT: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Input {
    id: u64,
    input_ns: u128,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Presented {
    pub id: u64,
    pub input_ns: u128,
    pub queued_ns: u128,
    pub presented_ns: u128,
    pub sequence: u64,
}

pub(crate) struct Trace {
    enabled: bool,
    count: u64,
    pending: Vec<Input>,
    in_flight: Option<(u128, Vec<Input>)>,
}

impl Trace {
    pub fn from_env() -> Self {
        Self::new(std::env::var("ROOST_PERF_TRACE").as_deref() == Ok("1"))
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            count: 0,
            pending: Vec::new(),
            in_flight: None,
        }
    }

    pub fn input(&mut self, at: Duration) {
        if !self.enabled || self.count >= LIMIT {
            return;
        }
        self.count += 1;
        let input = Input {
            id: self.count,
            input_ns: at.as_nanos(),
        };
        self.pending.push(input);
        eprintln!(
            "roost-perf-input: {{\"kind\":\"input\",\"id\":{},\"input_ns\":{}}}",
            input.id, input.input_ns
        );
    }

    /// Called only after the primary output's actual queue_buffer succeeds.
    pub fn queued(&mut self, at: Duration) {
        if !self.enabled || self.pending.is_empty() {
            return;
        }
        if self.in_flight.is_some() {
            // Never assign an event to an ambiguous/replaced pending frame.
            self.cancel();
            return;
        }
        let inputs = std::mem::take(&mut self.pending);
        let queued_ns = at.as_nanos();
        for input in &inputs {
            eprintln!(
                "roost-perf-input: {{\"kind\":\"queued\",\"id\":{},\"queued_ns\":{}}}",
                input.id, queued_ns
            );
        }
        self.in_flight = Some((queued_ns, inputs));
    }

    /// A real primary page flip; absent kernel time cannot measure latency.
    pub fn presented(&mut self, at: Option<Duration>, sequence: u64) -> Vec<Presented> {
        let Some((queued_ns, inputs)) = self.in_flight.take() else {
            return Vec::new();
        };
        let Some(at) = at else {
            self.discard(&inputs);
            return Vec::new();
        };
        let presented_ns = at.as_nanos();
        if inputs
            .iter()
            .any(|input| input.input_ns > queued_ns || queued_ns > presented_ns)
        {
            self.discard(&inputs);
            return Vec::new();
        }
        inputs.into_iter().map(|input| {
            let row = Presented { id: input.id, input_ns: input.input_ns, queued_ns, presented_ns, sequence };
            eprintln!("roost-perf-input: {{\"kind\":\"presented\",\"id\":{},\"input_ns\":{},\"queued_ns\":{},\"presented_ns\":{},\"sequence\":{}}}", row.id, row.input_ns, row.queued_ns, row.presented_ns, row.sequence);
            row
        }).collect()
    }

    pub fn cancel(&mut self) {
        self.discard(&self.pending);
        self.pending.clear();
        if let Some((_, inputs)) = self.in_flight.take() {
            self.discard(&inputs);
        }
    }

    fn discard(&self, inputs: &[Input]) {
        for input in inputs {
            eprintln!(
                "roost-perf-input: {{\"kind\":\"discarded\",\"id\":{}}}",
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

    #[test]
    fn a_pending_old_frame_cannot_complete_a_new_input() {
        let mut trace = Trace::new(true);
        trace.input(ns(10));
        assert!(trace.presented(Some(ns(20)), 1).is_empty());
        trace.queued(ns(30));
        assert_eq!(
            trace.presented(Some(ns(40)), 2),
            vec![Presented {
                id: 1,
                input_ns: 10,
                queued_ns: 30,
                presented_ns: 40,
                sequence: 2,
            }]
        );
        assert!(trace.presented(Some(ns(50)), 3).is_empty());
    }

    #[test]
    fn lock_reset_and_unknown_or_stale_kernel_times_never_report_latency() {
        let mut trace = Trace::new(true);
        trace.input(ns(10));
        trace.queued(ns(20));
        trace.cancel();
        assert!(trace.presented(Some(ns(30)), 1).is_empty());
        trace.input(ns(40));
        trace.queued(ns(50));
        assert!(trace.presented(None, 2).is_empty());
        trace.input(ns(60));
        trace.queued(ns(70));
        assert!(trace.presented(Some(ns(65)), 3).is_empty());
    }

    #[test]
    fn coalesced_inputs_share_only_the_frame_that_was_actually_queued() {
        let mut trace = Trace::new(true);
        trace.input(ns(10));
        trace.input(ns(15));
        trace.queued(ns(20));
        trace.input(ns(25));
        let rows = trace.presented(Some(ns(30)), 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [1, 2]);
        trace.queued(ns(40));
        assert_eq!(trace.presented(Some(ns(50)), 2)[0].id, 3);
    }

    #[test]
    fn disabled_trace_and_fixed_lifetime_limit_bound_collection() {
        let mut trace = Trace::new(false);
        trace.input(ns(1));
        trace.queued(ns(2));
        assert!(trace.presented(Some(ns(3)), 1).is_empty());
        let mut trace = Trace::new(true);
        for _ in 0..100 {
            trace.input(ns(1));
        }
        assert_eq!(trace.count, LIMIT);
        assert_eq!(trace.pending.len(), LIMIT as usize);
    }
}
