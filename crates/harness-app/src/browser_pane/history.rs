//! Per-project browsing history: the pages browser tabs opened, one list per
//! project (repository), shared by all its worktrees, branches and tasks.
//!
//! **What's recorded.** Top-level page visits in browser tabs, http(s) only
//! ([`recordable`]), when a navigation commits or the page changes its URL in
//! place (`pushState`). A title arriving later updates its entry.
//!
//! **Shape.** Newest first, one entry per URL (normalized by `url::Url`,
//! path untouched), at most [`MAX_ENTRIES`]; the oldest fall off.
//!
//! **Storage.** `~/.openorchestra/browser-history/<project file>.json`
//! ([`file_name`]), written with `state_files::write_private`. The
//! [`BrowserHistory`] entity keeps every loaded project in memory, records
//! at once, and writes the changed projects [`SAVE_DELAY`] after the first
//! unsaved change; quit flushes ([`BrowserHistory::flush`]). Older builds
//! never look in that folder.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{Context, Task};
use serde::{Deserialize, Serialize};

/// Most entries one project keeps.
pub(crate) const MAX_ENTRIES: usize = 500;

/// Most suggestions the address bar shows.
pub(crate) const MAX_SUGGESTIONS: usize = 8;

/// How long after the first unsaved change history is written.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// One page in a project's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryEntry {
    pub url: String,
    /// The page's title; empty until one arrives.
    #[serde(default)]
    pub title: String,
    /// Unix seconds.
    pub last_visited: u64,
    pub visits: u32,
}

/// A project's history, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

/// The URL to record for a top-level page, or `None` for one that isn't
/// recorded: anything but http(s) (`about:blank`, `data:`, `blob:`,
/// `file:` …) or a URL `url::Url` can't parse. The result is `url::Url`'s
/// serialization (lower-case scheme and host, no default port), which is
/// what entries are deduplicated by; the path is left as the page has it.
///
/// Secrets never reach the history file or the lists that show it
/// (ruling R16): `user:password@` is dropped, and so is a fragment that
/// carries a token (`access_token`, `id_token`, `token=`, any case), as an
/// OAuth redirect's does. Other fragments stay: hash routers need them.
pub(crate) fn recordable(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    if !(matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|h| !h.is_empty()))
    {
        return None;
    }
    // Can't fail for an http(s) URL with a host.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    if parsed.fragment().is_some_and(|f| {
        let f = f.to_ascii_lowercase();
        ["access_token", "id_token", "token="].iter().any(|t| f.contains(t))
    }) {
        parsed.set_fragment(None);
    }
    Some(parsed.into())
}

impl History {
    /// A visit to `url` at `now`: its entry moves to the front with one more
    /// visit, or a new one is added there. Returns whether anything changed
    /// (`false` for a URL that isn't [`recordable`]).
    pub(crate) fn record(&mut self, url: &str, now: u64) -> bool {
        let Some(url) = recordable(url) else {
            return false;
        };
        let mut entry = match self.entries.iter().position(|e| e.url == url) {
            Some(i) => self.entries.remove(i),
            None => HistoryEntry {
                url,
                title: String::new(),
                last_visited: now,
                visits: 0,
            },
        };
        entry.last_visited = now;
        entry.visits = entry.visits.saturating_add(1);
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        true
    }

    /// The `n` newest entries, for the blank tab's Recent list.
    pub(crate) fn recent(&self, n: usize) -> &[HistoryEntry] {
        &self.entries[..n.min(self.entries.len())]
    }

    /// Whether `url` is already the newest entry: seeing it again is a
    /// reload, or a tab's page coming back when the tab is shown again, not
    /// another visit.
    pub(crate) fn is_latest(&self, url: &str) -> bool {
        recordable(url).is_some_and(|url| self.entries.first().is_some_and(|e| e.url == url))
    }

    /// Give `url`'s entry `title`. Returns whether it changed; a URL with no
    /// entry, or an empty title, changes nothing.
    pub(crate) fn set_title(&mut self, url: &str, title: &str) -> bool {
        let title = title.trim();
        let Some(url) = recordable(url) else {
            return false;
        };
        match self.entries.iter_mut().find(|e| e.url == url) {
            Some(e) if !title.is_empty() && e.title != title => {
                e.title = title.to_string();
                true
            }
            _ => false,
        }
    }

    /// Up to `limit` entries matching what was typed, best first: entries
    /// whose host starts with the query first, then by recency × visits
    /// ([`score`]). A match is a case-insensitive substring of the URL or
    /// the title. An empty query matches nothing.
    pub(crate) fn suggest(&self, query: &str, now: u64, limit: usize) -> Vec<&HistoryEntry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(bool, f64, usize, &HistoryEntry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.url.to_lowercase().contains(&query) || e.title.to_lowercase().contains(&query)
            })
            .map(|(i, e)| (host_starts_with(&e.url, &query), score(e, now), i, e))
            .collect();
        // Host-prefix matches first, then the higher score, then newest.
        hits.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.total_cmp(&a.1))
                .then(a.2.cmp(&b.2))
        });
        hits.into_iter().take(limit).map(|h| h.3).collect()
    }
}

/// Whether `url`'s host (`localhost:8765` counts as `localhost`), or its
/// text with the scheme dropped, starts with `query` (lower-case).
fn host_starts_with(url: &str, query: &str) -> bool {
    let bare = display_url(url).to_lowercase();
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    host.starts_with(query) || bare.starts_with(query)
}

/// How strongly to suggest `e` at `now`: its visits, halved for every day
/// since the last one. A page visited often last week can still beat one
/// seen once an hour ago, but not one used all morning.
fn score(e: &HistoryEntry, now: u64) -> f64 {
    let days = now.saturating_sub(e.last_visited) as f64 / 86_400.0;
    f64::from(e.visits.max(1)) * 0.5_f64.powf(days)
}

/// When a page was last visited, as the Recent list shows it: "just now",
/// "2m ago", "3h ago", "yesterday" (24–48 hours ago), then the date ("Nov 3",
/// with the year when it isn't this one). Dates are UTC.
pub(crate) fn relative_time(then: u64, now: u64) -> String {
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;
    let ago = now.saturating_sub(then);
    match ago {
        0..60 => "just now".to_string(),
        60..HOUR => format!("{}m ago", ago / 60),
        HOUR..DAY => format!("{}h ago", ago / HOUR),
        DAY..172_800 => "yesterday".to_string(),
        _ => {
            let date = |secs: u64| {
                i64::try_from(secs)
                    .ok()
                    .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
            };
            match (date(then), date(now)) {
                (Some(then), Some(now)) => {
                    let month = &then.month().to_string()[..3];
                    if then.year() == now.year() {
                        format!("{month} {}", then.day())
                    } else {
                        format!("{month} {}, {}", then.day(), then.year())
                    }
                }
                _ => String::new(),
            }
        }
    }
}

/// A URL without its scheme, as the suggestions and the Recent list show
/// it: `localhost:8765/second.html`.
pub(crate) fn display_url(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

/// The file a project's history lives in, from its key (the main
/// checkout's path): the key with every run of characters other than
/// ASCII letters, digits, `.` and `_` turned into one `-`, trimmed of
/// `-`/`.` at the ends and capped in length, then a hash of the whole key,
/// so two keys that sanitize alike (`/a/b-c`, `/a/b/c`) never share a file.
pub(crate) fn file_name(project_key: &str) -> String {
    const MAX_STEM: usize = 80;
    let mut stem = String::new();
    for c in project_key.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            stem.push(c);
        } else if !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_matches(['-', '.']);
    // Keep the end: the project's own folder name is the useful part.
    let stem = &stem[stem.len().saturating_sub(MAX_STEM)..];
    let stem = stem.trim_start_matches(['-', '.']);
    let stem = if stem.is_empty() { "project" } else { stem };
    format!("{stem}-{:016x}.json", fnv1a(project_key.as_bytes()))
}

/// FNV-1a, 64-bit: a hash that stays the same across Rust versions, which
/// `DefaultHasher` doesn't promise, for file names that must too.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Read `project`'s history from `dir`; missing or unreadable is empty.
fn load(dir: &Path, project: &str) -> History {
    std::fs::read(dir.join(file_name(project)))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write `project`'s history into `dir`, private and atomic.
fn save(dir: &Path, project: &str, history: &History) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(history).map_err(std::io::Error::other)?;
    crate::state_files::write_private(&dir.join(file_name(project)), &bytes)
}

/// Delete every project's history file: the whole folder. Already gone is
/// fine.
fn remove_all(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Every project's history, shared by the browser tabs (which record and
/// suggest) and the workspace (which flushes on quit and clears from
/// Settings).
pub(crate) struct BrowserHistory {
    /// `~/.openorchestra/browser-history`; a temp folder in tests.
    dir: PathBuf,
    /// Loaded projects, by key.
    projects: HashMap<String, History>,
    /// Projects changed since their last write.
    dirty: HashSet<String>,
    /// The pending write. Started by the first unsaved change and never
    /// restarted by later ones, so a page animating its title can't hold
    /// the write off forever.
    save: Option<Task<()>>,
}

impl BrowserHistory {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            projects: HashMap::new(),
            dirty: HashSet::new(),
            save: None,
        }
    }

    /// Load `project` from disk if it isn't yet.
    pub(crate) fn ensure_loaded(&mut self, project: &str) {
        if !self.projects.contains_key(project) {
            let history = load(&self.dir, project);
            self.projects.insert(project.to_string(), history);
        }
    }

    /// `project`'s history, if loaded ([`Self::ensure_loaded`]).
    pub(crate) fn get(&self, project: &str) -> Option<&History> {
        self.projects.get(project)
    }

    /// A visit to `url` in `project` now, unless it is already the
    /// project's latest page ([`History::is_latest`]).
    pub(crate) fn record(&mut self, project: &str, url: &str, cx: &mut Context<Self>) {
        self.ensure_loaded(project);
        let now = unix_now();
        if self
            .projects
            .get_mut(project)
            .is_some_and(|h| !h.is_latest(url) && h.record(url, now))
        {
            self.changed(project, cx);
        }
    }

    /// The title of `url` in `project` arrived or changed.
    pub(crate) fn set_title(
        &mut self,
        project: &str,
        url: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        if self
            .projects
            .get_mut(project)
            .is_some_and(|h| h.set_title(url, title))
        {
            self.changed(project, cx);
        }
    }

    fn changed(&mut self, project: &str, cx: &mut Context<Self>) {
        self.dirty.insert(project.to_string());
        if self.save.is_none() {
            self.save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SAVE_DELAY).await;
                let _ = this.update(cx, |this, _| this.flush());
            }));
        }
        cx.notify();
    }

    /// Write every changed project now (quit, or the pending write firing).
    pub(crate) fn flush(&mut self) {
        self.save = None;
        for project in std::mem::take(&mut self.dirty) {
            let Some(history) = self.projects.get(&project) else {
                continue;
            };
            if let Err(e) = save(&self.dir, &project, history) {
                log::warn!("browser history: could not write {project}'s: {e}");
            }
        }
    }

    /// Forget every project's history, in memory and on disk, including a
    /// write still pending (it would bring a project back).
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) -> std::io::Result<()> {
        self.save = None;
        self.dirty.clear();
        self.projects.clear();
        cx.notify();
        remove_all(&self.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titled(url: &str, title: &str, last_visited: u64, visits: u32) -> HistoryEntry {
        HistoryEntry {
            url: url.into(),
            title: title.into(),
            last_visited,
            visits,
        }
    }

    fn urls(entries: &[HistoryEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.url.as_str()).collect()
    }

    #[test]
    fn only_http_and_https_pages_are_recorded() {
        assert_eq!(
            recordable("http://localhost:8765/"),
            Some("http://localhost:8765/".into())
        );
        assert_eq!(
            recordable("https://Example.COM:443/A/b?q=1#x"),
            Some("https://example.com/A/b?q=1#x".into())
        );
        for url in [
            "about:blank",
            "data:text/html,hi",
            "blob:http://localhost:3000/1234",
            "file:///etc/hosts",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "not a url",
            "",
        ] {
            assert_eq!(recordable(url), None, "{url}");
        }
    }

    #[test]
    fn credentials_never_reach_history() {
        assert_eq!(
            recordable("http://user:secret@localhost:8765/admin"),
            Some("http://localhost:8765/admin".into())
        );
        assert_eq!(
            recordable("https://user@example.com/"),
            Some("https://example.com/".into())
        );
        let mut h = History::default();
        h.record("http://user:secret@localhost:8765/", 1);
        assert!(!h.entries[0].url.contains("secret"));
        assert!(h.is_latest("http://localhost:8765/"));
    }

    #[test]
    fn token_fragments_are_dropped_and_router_fragments_kept() {
        for (given, kept) in [
            (
                "https://app.example.com/cb#access_token=abc&token_type=bearer",
                "https://app.example.com/cb",
            ),
            (
                "https://app.example.com/cb#id_token=eyJ&state=1",
                "https://app.example.com/cb",
            ),
            ("http://localhost:3000/#/login?token=xyz", "http://localhost:3000/"),
            ("http://localhost:3000/#ID_TOKEN=x", "http://localhost:3000/"),
            // Hash routers and anchors keep theirs.
            ("http://localhost:3000/#/settings", "http://localhost:3000/#/settings"),
            ("https://docs.rs/url#examples", "https://docs.rs/url#examples"),
            ("http://localhost:3000/#/tokens", "http://localhost:3000/#/tokens"),
        ] {
            assert_eq!(recordable(given).as_deref(), Some(kept), "{given}");
        }
    }

    #[test]
    fn a_visit_goes_to_the_front_once_per_url() {
        let mut h = History::default();
        assert!(h.record("http://localhost:8765/", 10));
        assert!(h.record("http://localhost:8765/second.html", 20));
        // The same page again, spelled differently: one entry, moved up.
        assert!(h.record("HTTP://LOCALHOST:8765/", 30));
        assert_eq!(
            urls(&h.entries),
            ["http://localhost:8765/", "http://localhost:8765/second.html"]
        );
        assert_eq!(h.entries[0].visits, 2);
        assert_eq!(h.entries[0].last_visited, 30);
        assert_eq!(h.entries[1].visits, 1);
        assert!(!h.record("about:blank", 40));
        assert_eq!(h.entries.len(), 2);
    }

    #[test]
    fn seeing_the_latest_page_again_is_not_a_visit() {
        let mut h = History::default();
        assert!(!h.is_latest("http://localhost:8765/"));
        h.record("http://localhost:8765/", 1);
        assert!(h.is_latest("http://LOCALHOST:8765/"));
        h.record("http://localhost:8765/second.html", 2);
        assert!(!h.is_latest("http://localhost:8765/"));
        assert!(!h.is_latest("about:blank"));
    }

    #[test]
    fn the_path_is_part_of_the_page() {
        let mut h = History::default();
        h.record("http://localhost:3000/a", 1);
        h.record("http://localhost:3000/A", 2);
        h.record("http://localhost:3000/a/", 3);
        assert_eq!(h.entries.len(), 3);
    }

    #[test]
    fn history_keeps_the_newest_five_hundred() {
        let mut h = History::default();
        for i in 0..(MAX_ENTRIES as u64 + 5) {
            h.record(&format!("http://localhost:3000/{i}"), i);
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert_eq!(h.entries[0].url, "http://localhost:3000/504");
        assert_eq!(h.entries.last().unwrap().url, "http://localhost:3000/5");
    }

    #[test]
    fn a_title_lands_on_its_page_s_entry() {
        let mut h = History::default();
        h.record("http://localhost:8765/", 1);
        assert!(h.set_title("http://localhost:8765/", " Browser Test Page "));
        assert_eq!(h.entries[0].title, "Browser Test Page");
        assert!(!h.set_title("http://localhost:8765/", "Browser Test Page"));
        assert!(!h.set_title("http://localhost:8765/", "  "));
        assert!(!h.set_title("http://localhost:9999/", "Elsewhere"));
        // A later visit keeps the title.
        h.record("http://localhost:8765/", 2);
        assert_eq!(h.entries[0].title, "Browser Test Page");
    }

    #[test]
    fn suggestions_match_url_or_title_ignoring_case() {
        let h = History {
            entries: vec![
                titled("http://localhost:8765/second.html", "Second Page", 100, 1),
                titled("http://localhost:8765/", "Browser Test Page", 90, 1),
                titled("https://docs.rs/url", "url - Rust", 80, 1),
            ],
        };
        let got = |q: &str| -> Vec<&str> {
            h.suggest(q, 100, MAX_SUGGESTIONS)
                .into_iter()
                .map(|e| e.url.as_str())
                .collect()
        };
        assert_eq!(got("TEST"), ["http://localhost:8765/"]);
        assert_eq!(got("second"), ["http://localhost:8765/second.html"]);
        assert_eq!(got("rust"), ["https://docs.rs/url"]);
        assert!(got("").is_empty());
        assert!(got("   ").is_empty());
        assert!(got("nothing-like-this").is_empty());
    }

    #[test]
    fn host_prefix_matches_come_first_then_recency_times_visits() {
        let day = 86_400;
        let now = 10 * day;
        let h = History {
            entries: vec![
                // Newest, but "local" only in its path.
                titled("https://example.com/local", "", now, 1),
                // Host starts with "local": visited once, a day ago.
                titled("http://localhost:3000/", "", now - day, 1),
                // Host starts with "local": visited often, an hour ago.
                titled("http://localhost:8765/", "", now - 3600, 6),
            ],
        };
        let got: Vec<&str> = h
            .suggest("local", now, MAX_SUGGESTIONS)
            .into_iter()
            .map(|e| e.url.as_str())
            .collect();
        assert_eq!(
            got,
            [
                "http://localhost:8765/",
                "http://localhost:3000/",
                "https://example.com/local",
            ]
        );
    }

    #[test]
    fn suggestions_stop_at_the_limit() {
        let mut h = History::default();
        for i in 0..20 {
            h.record(&format!("http://localhost:3000/{i}"), i);
        }
        let got = h.suggest("localhost", 20, MAX_SUGGESTIONS);
        assert_eq!(got.len(), MAX_SUGGESTIONS);
        // Equal visits: the newest first.
        assert_eq!(got[0].url, "http://localhost:3000/19");
    }

    #[test]
    fn visit_times_read_as_minutes_hours_yesterday_then_a_date() {
        // 2026-11-20T12:00:00Z
        let now = 1_795_176_000;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now - 59, now), "just now");
        // A clock that went back: not "in the future".
        assert_eq!(relative_time(now + 30, now), "just now");
        assert_eq!(relative_time(now - 60, now), "1m ago");
        assert_eq!(relative_time(now - 125, now), "2m ago");
        assert_eq!(relative_time(now - 3599, now), "59m ago");
        assert_eq!(relative_time(now - 3600, now), "1h ago");
        assert_eq!(relative_time(now - 3 * 3600 - 59, now), "3h ago");
        assert_eq!(relative_time(now - 86_399, now), "23h ago");
        assert_eq!(relative_time(now - 86_400, now), "yesterday");
        assert_eq!(relative_time(now - 2 * 86_400 + 1, now), "yesterday");
        assert_eq!(relative_time(now - 2 * 86_400, now), "Nov 18");
        assert_eq!(relative_time(now - 17 * 86_400, now), "Nov 3");
        // 2025-12-30T12:00:00Z: last year shows the year.
        assert_eq!(relative_time(1_767_096_000, now), "Dec 30, 2025");
    }

    #[test]
    fn the_recent_list_is_the_newest_few() {
        let mut h = History::default();
        for i in 0..12 {
            h.record(&format!("http://localhost:3000/{i}"), i);
        }
        let recent: Vec<&str> = h.recent(3).iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            recent,
            [
                "http://localhost:3000/11",
                "http://localhost:3000/10",
                "http://localhost:3000/9"
            ]
        );
        assert_eq!(History::default().recent(8).len(), 0);
    }

    #[test]
    fn urls_show_without_their_scheme() {
        assert_eq!(display_url("http://localhost:8765/"), "localhost:8765/");
        assert_eq!(
            display_url("https://example.com/a?b#c"),
            "example.com/a?b#c"
        );
        assert_eq!(display_url("localhost"), "localhost");
    }

    #[test]
    fn project_file_names_are_safe_and_distinct() {
        let a = file_name("/Users/omar/projects/omar-harness");
        assert!(a.starts_with("Users-omar-projects-omar-harness-"), "{a}");
        assert!(a.ends_with(".json"));
        assert_eq!(a, file_name("/Users/omar/projects/omar-harness"));
        // Keys that sanitize alike still get their own file.
        assert_ne!(file_name("/a/b-c"), file_name("/a/b/c"));
        for key in ["/", "", "..", "/../../etc/passwd", "a b/ü*?:\\"] {
            let name = file_name(key);
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{key:?} → {name}"
            );
            assert!(!name.starts_with('.') && !name.starts_with('-'), "{name}");
            assert!(!name.contains(".."), "{name}");
        }
        let long = format!("/{}", "deep/".repeat(100));
        assert!(file_name(&long).len() <= 80 + 1 + 16 + 5);
    }

    #[test]
    fn history_round_trips_through_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        // The folder doesn't exist yet: the first write makes it.
        let dir = tmp.path().join("browser-history");
        let mut h = History::default();
        h.record("http://localhost:8765/", 5);
        h.set_title("http://localhost:8765/", "Browser Test Page");
        save(&dir, "/repo/a", &h).unwrap();
        assert_eq!(load(&dir, "/repo/a"), h);
        assert_eq!(load(&dir, "/repo/b"), History::default());
        std::fs::write(dir.join(file_name("/repo/c")), b"{not json").unwrap();
        assert_eq!(load(&dir, "/repo/c"), History::default());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(file_name("/repo/a")))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn clearing_removes_every_project_s_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("browser-history");
        let mut h = History::default();
        h.record("http://localhost:8765/", 5);
        save(&dir, "/repo/a", &h).unwrap();
        save(&dir, "/repo/b", &h).unwrap();
        remove_all(&dir).unwrap();
        assert!(!dir.exists());
        assert_eq!(load(&dir, "/repo/a"), History::default());
        // Nothing to clear is fine too.
        remove_all(&dir).unwrap();
        // The state folder around it is left alone.
        assert!(tmp.path().exists());
    }
}
