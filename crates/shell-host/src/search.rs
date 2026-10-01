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

    /// Echo provider: every hit title carries the query text, so tests
    /// can tell which generation an answer belongs to.
    struct Echo {
        id: &'static str,
        delay: Duration,
        fail: bool,
    }

    impl SearchProvider for Echo {
        fn id(&self) -> &'static str {
            self.id
        }

        fn query(&self, text: &str) -> Vec<SearchResult> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            if self.fail {
                return Vec::new();
            }
            vec![SearchResult::launch(
                format!("{}:{text}", self.id),
                "app.id",
            )]
        }
    }

    fn hub_with(fast: bool, slow_delay: Duration, fail: bool) -> SearchHub {
        SearchHub::new(vec![
            Arc::new(Echo {
                id: "fast",
                delay: Duration::ZERO,
                fail: !fast,
            }),
            Arc::new(Echo {
                id: "slow",
                delay: slow_delay,
                fail,
            }),
        ])
    }

    #[test]
    fn blank_query_spawns_nothing_and_collects_empty() {
        let mut hub = hub_with(true, Duration::ZERO, false);
        let gen = hub.query("   ");
        assert_eq!(gen, 1);
        assert!(hub.collect().is_empty());
    }

    #[test]
    fn slow_provider_never_blocks_typing_or_first_results() {
        let mut hub = hub_with(true, Duration::from_millis(150), false);
        let first = hub.query("a");
        // The fast answer is already waiting; the slow one is not.
        let mut saw_fast = false;
        for _ in 0..100 {
            for hit in hub.collect() {
                if hit.title == "fast:a" {
                    saw_fast = true;
                }
                assert!(!hit.title.starts_with("slow:"), "slow answer is late");
            }
            if saw_fast {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(saw_fast, "fast provider answers while slow works");
        assert_eq!(hub.generation(), first);
    }

    #[test]
    fn late_slow_answer_joins_current_results() {
        let mut hub = hub_with(true, Duration::from_millis(50), false);
        hub.query("a");
        std::thread::sleep(Duration::from_millis(200));
        let titles: Vec<String> = hub.collect().iter().map(|h| h.title.clone()).collect();
        assert!(titles.contains(&"fast:a".to_owned()));
        assert!(titles.contains(&"slow:a".to_owned()));
    }

    #[test]
    fn new_query_supersedes_inflight_answers() {
        let mut hub = hub_with(true, Duration::from_millis(100), false);
        hub.query("a");
        hub.query("ab");
        std::thread::sleep(Duration::from_millis(300));
        let titles: Vec<String> = hub.collect().iter().map(|h| h.title.clone()).collect();
        assert!(
            !titles.iter().any(|t| t.ends_with(":a")),
            "gen-1 answers dropped"
        );
        assert!(titles.contains(&"fast:ab".to_owned()));
        assert!(titles.contains(&"slow:ab".to_owned()));
    }

    #[test]
    fn cancel_drops_everything_inflight() {
        let mut hub = hub_with(true, Duration::from_millis(100), false);
        hub.query("a");
        hub.cancel();
        std::thread::sleep(Duration::from_millis(200));
        assert!(hub.collect().is_empty());
    }

    #[test]
    fn failing_provider_yields_empty_without_breaking_others() {
        let mut hub = hub_with(true, Duration::ZERO, true);
        hub.query("a");
        std::thread::sleep(Duration::from_millis(50));
        let titles: Vec<String> = hub.collect().iter().map(|h| h.title.clone()).collect();
        assert_eq!(titles, vec!["fast:a".to_owned()]);
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
        std::thread::sleep(Duration::from_millis(100));
        let out = hub.collect();
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
