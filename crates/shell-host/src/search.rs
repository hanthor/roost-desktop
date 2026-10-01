//! Async bounded search over compositor truth and desktop entries.
//!
//! 002 T2 (Milestone 1): typing in the overview fans one query out to
//! every [`SearchProvider`] without ever waiting for one. Each query
//! bumps a generation counter and runs each provider on its own thread;
//! [`SearchHub::collect`] drains only the current generation over a
//! nonblocking channel, so a slow or dead provider degrades to missing
//! results instead of stalling typing, focus, or frames. There is no
//! async runtime in this crate — threads plus generations are the
//! whole mechanism, and every bound is a named constant.
//!
//! Result order follows provider registration order (each provider's
//! own hits capped first, then the merged list capped as a whole).
//! [`SearchAction::Focus`] carries a compositor window id back into the
//! token-gated activation path; [`SearchAction::Launch`] carries an
//! app id the launcher resolves.

use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};

use crate::model::{ShellModel, WindowEntry};

/// Results one provider may contribute to a single query.
pub const MAX_PROVIDER_RESULTS: usize = 8;
/// Results the merged answer may hold across all providers.
pub const MAX_TOTAL_RESULTS: usize = 16;

/// What activating a search result does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAction {
    /// Launch the app by desktop-entry id; the launcher resolves it.
    Launch {
        /// Desktop-entry id (e.g. `org.gnome.Terminal.desktop`).
        app_id: String,
    },
    /// Focus a live window through the token-gated activation path.
    Focus {
        /// Compositor window id.
        window: u64,
    },
}

/// One ranked hit from a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Display title (untrusted text: entry names and window titles).
    pub title: String,
    /// Owning app id, when the hit names an application.
    pub app_id: Option<String>,
    /// What Enter does with this hit.
    pub action: SearchAction,
}

impl SearchResult {
    /// Launch hit for an application.
    pub fn launch(title: impl Into<String>, app_id: impl Into<String>) -> Self {
        let app_id = app_id.into();
        Self {
            title: title.into(),
            app_id: Some(app_id.clone()),
            action: SearchAction::Launch { app_id },
        }
    }

    /// Focus hit for a live window.
    pub fn focus(title: impl Into<String>, app_id: Option<String>, window: u64) -> Self {
        Self {
            title: title.into(),
            app_id,
            action: SearchAction::Focus { window },
        }
    }
}

/// A synchronous result source behind the async hub.
///
/// Providers never block each other: the hub runs each one on its own
/// thread per query, so `query` may take as long as it needs. Stale
/// answers are discarded by generation, never by asking the provider
/// to stop mid-call — keep `query` side-effect free so abandoned runs
/// are harmless.
pub trait SearchProvider: Send + Sync {
    /// Stable short name used in diagnostics and result attribution.
    fn id(&self) -> &'static str;
    /// Hits for `text` (already trimmed by the hub; empty text never
    /// reaches providers). Honor [`MAX_PROVIDER_RESULTS`]... or return
    /// fewer; the hub caps regardless.
    fn query(&self, text: &str) -> Vec<SearchResult>;
}

/// Live-window provider: switch-to-instance from the control model.
///
/// Holds a snapshot refreshed from [`ShellModel`]. The hub owns its
/// providers behind `Arc` while the host refreshes the snapshot after
/// every sync, so the snapshot lives behind a lock and both sides
/// share one provider. Matching is a case-insensitive substring over
/// the window title.
#[derive(Debug, Default)]
pub struct WindowProvider {
    windows: std::sync::RwLock<Vec<WindowEntry>>,
}

impl WindowProvider {
    /// Empty provider; call [`refresh`](Self::refresh) before querying.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the snapshot with the model's current window list.
    pub fn refresh(&self, model: &ShellModel) {
        *self.windows.write().expect("window snapshot lock") = model.windows().to_vec();
    }
}

impl SearchProvider for WindowProvider {
    fn id(&self) -> &'static str {
        "windows"
    }

    fn query(&self, text: &str) -> Vec<SearchResult> {
        let needle = text.to_lowercase();
        self.windows
            .read()
            .expect("window snapshot lock")
            .iter()
            .filter(|w| w.title.to_lowercase().contains(&needle))
            .take(MAX_PROVIDER_RESULTS)
            .map(|w| SearchResult::focus(w.title.clone(), None, w.id))
            .collect()
    }
}

/// One provider's answer to one query generation.
#[derive(Debug)]
struct TaggedResults {
    generation: u64,
    results: Vec<SearchResult>,
}

/// Fans queries out to providers and merges bounded current answers.
///
/// Clone-free sharing: the hub is `Send + Sync` (`Arc` providers, an
/// atomic generation, an `mpsc` channel), so the panel loop can own it
/// while queries resolve on worker threads.
pub struct SearchHub {
    providers: Vec<Arc<dyn SearchProvider>>,
    generation: AtomicU64,
    tx: Sender<TaggedResults>,
    rx: Receiver<TaggedResults>,
    pending: VecDeque<SearchResult>,
    pending_generation: u64,
}

impl SearchHub {
    /// Hub over the given providers in merge order.
    pub fn new(providers: Vec<Arc<dyn SearchProvider>>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            providers,
            generation: AtomicU64::new(0),
            tx,
            rx,
            pending: VecDeque::new(),
            pending_generation: 0,
        }
    }

    /// Provider ids in merge order.
    pub fn provider_ids(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Start answering `text`, returning the query generation.
    ///
    /// Always returns immediately: one thread per provider, each
    /// fire-and-forget (a spawn failure just loses that provider's
    /// answer for this generation — fail-closed, typing stays live).
    /// Blank text answers nothing and spawns nothing; the favorites
    /// grid owns the empty state.
    pub fn query(&self, text: &str) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return generation;
        }
        for provider in &self.providers {
            let thread_provider = Arc::clone(provider);
            let tx = self.tx.clone();
            let thread_text = text.clone();
            let run = move || {
                let mut results = thread_provider.query(&thread_text);
                results.truncate(MAX_PROVIDER_RESULTS);
                let _ = tx.send(TaggedResults {
                    generation,
                    results,
                });
            };
            if std::thread::Builder::new()
                .name(format!("roost-search-{}", provider.id()))
                .spawn(run)
                .is_err()
            {
                // No thread budget left: this provider sits this query
                // out rather than blocking the keystroke that asked.
            }
        }
        generation
    }

    /// Drop every in-flight answer: the next [`collect`](Self::collect)
    /// sees only generations started after this call.
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Current query generation (the one [`collect`](Self::collect) keeps).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Nonblocking drain of current-generation answers, merged in
    /// provider order and capped at [`MAX_TOTAL_RESULTS`].
    ///
    /// Answers queue in `pending` across calls, so a slow provider's
    /// late arrival joins the current answer instead of replacing it;
    /// a newer [`query`](Self::query) or [`cancel`](Self::cancel)
    /// discards the whole queue with the old generation.
    pub fn collect(&mut self) -> Vec<SearchResult> {
        let current = self.generation();
        // Leftovers queued for an older generation never surface: a
        // newer query (or cancel) retires the whole queue.
        if self.pending_generation != current {
            self.pending.clear();
            self.pending_generation = current;
        }
        for tagged in self.rx.try_iter() {
            if tagged.generation != current {
                continue;
            }
            self.pending.extend(tagged.results);
        }
        if self.generation() != current {
            // A concurrent query flipped the generation mid-drain: the
            // queue may mix generations, so drop it and keep only what
            // arrived for the newest one.
            self.pending.clear();
            let newest = self.generation();
            self.pending_generation = newest;
            for tagged in self.rx.try_iter() {
                if tagged.generation == newest {
                    self.pending.extend(tagged.results);
                }
            }
        }
        let mut out = Vec::new();
        while out.len() < MAX_TOTAL_RESULTS {
            match self.pending.pop_front() {
                Some(result) => out.push(result),
                None => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use std::sync::atomic::AtomicUsize;
    use std::sync::{Condvar, Mutex};
    use std::time::Instant;

    /// Generous wall-clock bound for waits. Tests never *assert* on
    /// timing (#72): a slow runner only makes them take longer.
    const DEADLINE: Duration = Duration::from_secs(20);

    /// Release latch for the slow provider: its answers are held until
    /// the test opens the gate, so "fast arrives while slow works" holds
    /// by construction instead of by sleep length.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.cv.wait(open).unwrap();
            }
        }

        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    /// Echo provider: every hit title carries the query text, so tests
    /// can tell which generation an answer belongs to. `done` counts
    /// finished queries so tests can wait for workers deterministically.
    struct Echo {
        id: &'static str,
        gate: Option<Arc<Gate>>,
        fail: bool,
        done: Arc<AtomicUsize>,
    }

    impl SearchProvider for Echo {
        fn id(&self) -> &'static str {
            self.id
        }

        fn query(&self, text: &str) -> Vec<SearchResult> {
            if let Some(gate) = &self.gate {
                gate.wait();
            }
            let out = if self.fail {
                Vec::new()
            } else {
                vec![SearchResult::launch(
                    format!("{}:{text}", self.id),
                    "app.id",
                )]
            };
            self.done.fetch_add(1, Ordering::SeqCst);
            out
        }
    }

    struct Fixture {
        hub: SearchHub,
        gate: Arc<Gate>,
        done: Arc<AtomicUsize>,
    }

    /// Fast provider answers at once; the slow one waits on `gate`.
    fn fixture(fast: bool, gated: bool, fail: bool) -> Fixture {
        let gate = Arc::new(Gate::default());
        if !gated {
            gate.release();
        }
        let done = Arc::new(AtomicUsize::new(0));
        let hub = SearchHub::new(vec![
            Arc::new(Echo {
                id: "fast",
                gate: None,
                fail: !fast,
                done: done.clone(),
            }),
            Arc::new(Echo {
                id: "slow",
                gate: Some(gate.clone()),
                fail,
                done: done.clone(),
            }),
        ]);
        Fixture { hub, gate, done }
    }

    /// Accumulate titles across draining `collect` calls until `want`
    /// holds or the deadline passes; returns everything seen.
    fn collect_until(hub: &mut SearchHub, want: impl Fn(&[String]) -> bool) -> Vec<String> {
        let start = Instant::now();
        let mut seen = Vec::new();
        loop {
            seen.extend(hub.collect().into_iter().map(|h| h.title));
            if want(&seen) || start.elapsed() > DEADLINE {
                return seen;
            }
            std::thread::yield_now();
        }
    }

    fn has(seen: &[String], title: &str) -> bool {
        seen.iter().any(|t| t == title)
    }

    /// Wait until `n` provider queries have finished.
    fn wait_done(done: &AtomicUsize, n: usize) {
        let start = Instant::now();
        while done.load(Ordering::SeqCst) < n {
            assert!(start.elapsed() < DEADLINE, "providers never finished");
            std::thread::yield_now();
        }
    }

    #[test]
    fn blank_query_spawns_nothing_and_collects_empty() {
        let mut f = fixture(true, false, false);
        let gen = f.hub.query("   ");
        assert_eq!(gen, 1);
        assert!(f.hub.collect().is_empty());
        assert_eq!(f.done.load(Ordering::SeqCst), 0, "blank spawns nothing");
    }

    #[test]
    fn slow_provider_never_blocks_typing_or_first_results() {
        let mut f = fixture(true, true, false);
        let first = f.hub.query("a");
        // The slow provider is held at its gate: the fast answer must
        // surface on its own.
        let seen = collect_until(&mut f.hub, |s| has(s, "fast:a"));
        assert!(
            has(&seen, "fast:a"),
            "fast provider answers while slow works"
        );
        assert!(!seen.iter().any(|t| t.starts_with("slow:")));
        assert_eq!(f.hub.generation(), first);
        f.gate.release();
    }

    #[test]
    fn late_slow_answer_joins_current_results() {
        let mut f = fixture(true, true, false);
        f.hub.query("a");
        let mut seen = collect_until(&mut f.hub, |s| has(s, "fast:a"));
        f.gate.release();
        seen.extend(collect_until(&mut f.hub, |s| has(s, "slow:a")));
        assert!(has(&seen, "fast:a"));
        assert!(has(&seen, "slow:a"));
    }

    #[test]
    fn new_query_supersedes_inflight_answers() {
        let mut f = fixture(true, true, false);
        f.hub.query("a");
        f.hub.query("ab");
        f.gate.release();
        let seen = collect_until(&mut f.hub, |s| has(s, "fast:ab") && has(s, "slow:ab"));
        // All four workers (two generations x two providers) finished.
        wait_done(&f.done, 4);
        let mut seen = seen;
        seen.extend(f.hub.collect().into_iter().map(|h| h.title));
        assert!(
            !seen.iter().any(|t| t.ends_with(":a")),
            "gen-1 answers dropped"
        );
        assert!(has(&seen, "fast:ab"));
        assert!(has(&seen, "slow:ab"));
    }

    #[test]
    fn cancel_drops_everything_inflight() {
        let mut f = fixture(true, true, false);
        f.hub.query("a");
        f.hub.cancel();
        f.gate.release();
        wait_done(&f.done, 2);
        // Workers finished; give their sends a moment to land, then the
        // drain must still be empty (the generation retired them).
        for _ in 0..50 {
            assert!(f.hub.collect().is_empty());
            std::thread::yield_now();
        }
    }

    #[test]
    fn failing_provider_yields_empty_without_breaking_others() {
        let mut f = fixture(true, false, true);
        f.hub.query("a");
        wait_done(&f.done, 2);
        let seen = collect_until(&mut f.hub, |s| has(s, "fast:a"));
        assert_eq!(seen, vec!["fast:a".to_owned()]);
    }

    #[test]
    fn merged_results_are_bounded() {
        struct Flood;
        impl SearchProvider for Flood {
            fn id(&self) -> &'static str {
                "flood"
            }
            fn query(&self, _text: &str) -> Vec<SearchResult> {
                (0..1000)
                    .map(|i| SearchResult::launch(format!("hit{i}"), "flood.app"))
                    .collect()
            }
        }
        let mut hub = SearchHub::new(vec![Arc::new(Flood)]);
        hub.query("x");
        let start = Instant::now();
        let out = loop {
            let out = hub.collect();
            if !out.is_empty() || start.elapsed() > DEADLINE {
                break out;
            }
            std::thread::yield_now();
        };
        assert!(!out.is_empty(), "flood answered");
        assert!(out.len() <= MAX_TOTAL_RESULTS);
        assert!(
            out.len() <= MAX_PROVIDER_RESULTS,
            "per-provider cap applies"
        );
    }

    #[test]
    fn window_provider_matches_titles_case_insensitively() {
        use crate::model::WindowEntry;
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                WindowEntry::new(1, "Terminal", true),
                WindowEntry::new(2, "Web Browser", false),
                WindowEntry::new(3, "Editor", false),
            ],
            vec![0],
        );
        let provider = WindowProvider::new();
        assert!(
            provider.query("term").is_empty(),
            "stale snapshot matches nothing"
        );
        provider.refresh(&model);
        let hits = provider.query("TERM");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].action, SearchAction::Focus { window: 1 });
        let hits = provider.query("e");
        assert!(hits.len() >= 2, "substring matches several windows");
        assert!(hits
            .iter()
            .all(|h| matches!(h.action, SearchAction::Focus { .. })));
    }
}
