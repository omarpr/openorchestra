//! The always-on conductor: a 3D maestro who stays on screen, beats time
//! while an agent works, holds his baton up while one waits on you, bows when
//! one finishes, and narrates what it is doing ("(app) Editing main.rs · 42s").
//!
//! Split three ways so most of it is testable without a window:
//! - [`scene`] renders the frames (pure CPU, no gpui);
//! - [`activity`] reads what a Claude Code agent is doing from its transcript;
//! - this module is the state machine: which agents are active, which one he
//!   is following, which frame to show, and what the caption says. The
//!   workspace drives it with a timer and renders [`View`].

pub mod activity;
pub mod scene;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_runtime::{AgentKind, AgentState, WaitReason};

use scene::{BOW_FRAMES, CONDUCT_FRAMES, Pose};

/// One frame of the beat pattern. 24 per bar ≈ 143 bpm in 4/4: brisk, but
/// slow enough that the arc reads as a gesture rather than a flicker.
const BEAT_FRAME: Duration = Duration::from_millis(70);
const BOW_FRAME: Duration = Duration::from_millis(85);
const BLINK: Duration = Duration::from_millis(140);
/// How many agents the bubble lists before summarizing the rest.
pub const MAX_LINES: usize = 3;
/// How often the visible agents' transcripts are re-read.
const ACTIVITY_EVERY: Duration = Duration::from_millis(900);
/// How long a "finished" / "needs you" announcement holds its line.
const ANNOUNCE_FOR: Duration = Duration::from_secs(8);

/// A terminal tab, or the worktree-wide status of a legacy writer.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AgentTarget {
    pub root: PathBuf,
    pub tab_id: Option<u64>,
}

impl AgentTarget {
    pub fn new(root: &Path, tab_id: Option<u64>) -> Self {
        Self { root: root.to_path_buf(), tab_id }
    }
}

/// What the conductor knows about one agent.
struct Tracked {
    kind: AgentKind,
    /// "(repo)" or "(repo · worktree)", resolved when the signal arrives.
    label: String,
    state: AgentState,
    reason: WaitReason,
    /// When the current turn started; the caption's counter.
    since: Option<Instant>,
    session_file: Option<PathBuf>,
    /// The tool its hook last said it was about to run.
    tool: Option<agent_runtime::ToolCall>,
}

/// An event he's announcing for one agent, in place of its usual line.
struct Announcement {
    target: AgentTarget,
    label: String,
    text: String,
    nav: String,
    until: Instant,
}

/// What to draw this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Index into the rendered frames.
    pub frame: usize,
    /// At most [`MAX_LINES`], announcements first, then the agent you're
    /// looking at, then the rest (waiting on you before running).
    pub lines: Vec<Caption>,
    /// Active agents that didn't fit.
    pub more: usize,
    pub running: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    /// "(repo)" / "(repo · worktree)".
    pub label: String,
    pub text: String,
    /// Where clicking the line goes (an alert nav id); empty for nowhere.
    pub nav: String,
    /// A "finished" / "needs you" event rather than a live status.
    pub announcement: bool,
    /// Worth interrupting you for: an announcement, or an agent waiting on
    /// you. Live "working…" lines are not.
    pub urgent: bool,
}

impl View {
    /// Whether the bubble has something for you, rather than only progress.
    pub fn needs_you(&self) -> bool {
        self.lines.iter().any(|l| l.urgent)
    }
}

pub struct Conductor {
    agents: HashMap<AgentTarget, Tracked>,
    /// Newest first.
    announcements: Vec<Announcement>,
    bow_started: Option<Instant>,
    clock: Instant,
    blink_until: Option<Instant>,
    next_blink: Instant,
    seed: u64,
    activity: HashMap<AgentTarget, String>,
    activity_read: Option<Instant>,
    /// Waiting agents you've already gone to see: they leave the bubble
    /// (and stop his cue) until they wait on you again.
    seen: HashSet<AgentTarget>,
    /// The agent tab you were looking at last tick.
    last_selected: Option<AgentTarget>,
}

impl Default for Conductor {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            agents: HashMap::new(),
            announcements: Vec::new(),
            bow_started: None,
            clock: now,
            blink_until: None,
            next_blink: now + Duration::from_millis(1800),
            seed: 0x9E37_79B9_7F4A_7C15,
            activity: HashMap::new(),
            activity_read: None,
            seen: HashSet::new(),
            last_selected: None,
        }
    }
}

/// "42s", "1m 23s", "2h 05m".
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

impl Conductor {
    /// Record an agent's state change. `label` says who it is.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        target: &AgentTarget,
        kind: AgentKind,
        state: AgentState,
        reason: WaitReason,
        session_file: Option<PathBuf>,
        tool: Option<agent_runtime::ToolCall>,
        label: String,
        now: Instant,
    ) -> Option<AgentState> {
        // A reattached hook can start writing tab records mid-turn after an
        // app update. Move its legacy status across when the transcript matches.
        let legacy = AgentTarget::new(&target.root, None);
        let legacy_state = if target.tab_id.is_some() && session_file.is_some()
            && self.agents.get(&legacy).is_some_and(|tracked| tracked.kind == kind && tracked.session_file == session_file)
        {
            self.remove(&legacy)
        } else {
            None
        };
        let previous = self.state(target).or(legacy_state.as_ref().map(|tracked| tracked.state));
        let t = self.agents.entry(target.clone()).or_insert_with(|| Tracked {
            kind: kind.clone(),
            label: label.clone(),
            state,
            reason,
            since: legacy_state.and_then(|tracked| tracked.since),
            session_file: None,
            tool: None,
        });
        t.kind = kind;
        t.label = label;
        t.reason = reason;
        if session_file.is_some() {
            t.session_file = session_file;
        }
        t.tool = tool;
        // Re-read promptly: a new tool is news.
        self.activity_read = None;
        t.since = match state {
            // A turn's clock starts on its first Running and keeps going
            // through Claude's repeated PreToolUse refreshes and through a
            // permission wait mid-turn. A wait that follows an idle (the
            // agent parked at its prompt) starts nothing.
            AgentState::Running => t.since.or(Some(now)),
            AgentState::Waiting => t.since,
            _ => None,
        };
        if state != AgentState::Running {
            self.activity.remove(target);
        }
        // Seeing a wait only dismisses that wait: a fresh one (or the agent
        // moving on) brings it back. Repeated Waiting refreshes keep it seen.
        if state != AgentState::Waiting || t.state != AgentState::Waiting {
            self.seen.remove(target);
        }
        // Back to work: a stale "finished" line for it would now be a lie.
        if state == AgentState::Running {
            self.announcements.retain(|a| &a.target != target);
        }
        t.state = state;
        previous
    }

    pub fn state(&self, target: &AgentTarget) -> Option<AgentState> {
        self.agents.get(target).map(|tracked| tracked.state)
    }

    pub fn selection_target(&self, target: AgentTarget, kind: &AgentKind) -> AgentTarget {
        if !self.agents.contains_key(&target) {
            let legacy = AgentTarget::new(&target.root, None);
            if self.agents.get(&legacy).is_some_and(|tracked| &tracked.kind == kind) {
                return legacy;
            }
        }
        target
    }

    pub fn root_state(&self, root: &Path) -> AgentState {
        self.agents.iter().filter(|(target, _)| target.root == root)
            .map(|(_, tracked)| tracked.state)
            .max_by_key(|state| match state {
                AgentState::Unknown => 0,
                AgentState::Idle => 1,
                AgentState::Running => 2,
                AgentState::Waiting => 3,
            })
            .unwrap_or(AgentState::Unknown)
    }

    pub fn active_targets(&self) -> impl Iterator<Item = &AgentTarget> {
        self.agents.iter().filter_map(|(target, tracked)| {
            matches!(tracked.state, AgentState::Running | AgentState::Waiting).then_some(target)
        })
    }

    /// Closing a tab or losing its process is different from a finished turn.
    pub fn stopped(&mut self, target: &AgentTarget) {
        self.remove(target);
    }

    fn remove(&mut self, target: &AgentTarget) -> Option<Tracked> {
        self.activity.remove(target);
        self.seen.remove(target);
        self.announcements.retain(|a| &a.target != target);
        self.agents.remove(target)
    }

    pub fn forget_root(&mut self, root: &Path) {
        let targets: Vec<_> = self.agents.keys().filter(|target| target.root == root).cloned().collect();
        for target in targets {
            self.stopped(&target);
        }
    }

    /// Show `text` for `root`'s agent for a while. A finish also takes a bow.
    pub fn announce(&mut self, target: &AgentTarget, label: String, text: String, nav: String, bow: bool, now: Instant) {
        self.announcements.retain(|a| &a.target != target);
        self.announcements.insert(
            0,
            Announcement {
                target: target.clone(),
                label,
                text,
                nav,
                until: now + ANNOUNCE_FOR,
            },
        );
        if bow {
            self.bow_started = Some(now);
        }
    }

    /// You checked on `root`'s agent (navigated to it, or clicked its line):
    /// drop its "finished" line, and its "needs you" until it next waits.
    pub fn dismiss(&mut self, target: &AgentTarget) {
        self.announcements.retain(|a| &a.target != target);
        if self.agents.get(target).is_some_and(|t| t.state == AgentState::Waiting) {
            self.seen.insert(target.clone());
        }
    }

    /// Waiting on you, and not yet seen.
    fn waiting(&self, path: &AgentTarget, t: &Tracked) -> bool {
        t.state == AgentState::Waiting && !self.seen.contains(path)
    }

    /// Active agents in display order: the one you're looking at, then those
    /// waiting on you, then the running ones; ties by path, so lines don't
    /// shuffle between ticks.
    fn active(&self, selected: Option<&AgentTarget>) -> Vec<AgentTarget> {
        let mut v: Vec<&AgentTarget> = self
            .agents
            .iter()
            .filter(|(p, t)| t.state == AgentState::Running || self.waiting(p, t))
            .map(|(p, _)| p)
            .collect();
        v.sort_by_key(|p| {
            (
                Some(*p) != selected,
                self.agents[*p].state != AgentState::Waiting,
                (*p).clone(),
            )
        });
        v.into_iter().cloned().collect()
    }

    fn rand(&mut self) -> u64 {
        // xorshift: plenty for blink timing.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    fn status_text(&self, path: &AgentTarget, now: Instant) -> String {
        let t = &self.agents[path];
        let elapsed = t.since.map(|s| format_elapsed(now.duration_since(s)));
        match t.state {
            AgentState::Waiting => match t.reason {
                WaitReason::PermissionPrompt => "Needs your OK".to_string(),
                _ => "Waiting on you".to_string(),
            },
            _ => {
                let fallback = if t.kind == AgentKind::ClaudeCode { "Thinking" } else { "Working" };
                let doing = self.activity.get(path).map_or(fallback, String::as_str);
                match (doing, &elapsed) {
                    ("Thinking" | "Working", Some(e)) => format!("{doing} for {e}…"),
                    (_, Some(e)) => format!("{doing}… · {e}"),
                    (_, None) => format!("{doing}…"),
                }
            }
        }
    }

    /// Advance timers and work out what to draw. `selected` is the worktree
    /// the user is looking at; `nav_for` maps an agent's root to its alert id.
    pub fn tick(&mut self, now: Instant, selected: Option<&AgentTarget>, nav_for: impl Fn(&AgentTarget, &AgentKind) -> String) -> View {
        // Blinking: a short close every 2–5.5 s, now and then a double.
        if self.blink_until.is_some_and(|u| now >= u) {
            self.blink_until = None;
        }
        if self.blink_until.is_none() && now >= self.next_blink {
            self.blink_until = Some(now + BLINK);
            let r = self.rand();
            self.next_blink = if r.is_multiple_of(5) {
                now + Duration::from_millis(320)
            } else {
                now + Duration::from_millis(2000 + r % 3500)
            };
        }
        let eyes_open = self.blink_until.is_none();
        self.announcements.retain(|a| now < a.until);
        // Arriving at an agent is checking on it. Only the arrival counts: a
        // wait that starts while you're already there still shows.
        if selected != self.last_selected.as_ref() {
            self.last_selected = selected.cloned();
            if let Some(p) = selected {
                self.dismiss(p);
            }
        }

        // Announcements take their agent's place; everyone else follows.
        let active: Vec<AgentTarget> = self
            .active(selected)
            .into_iter()
            .filter(|p| !self.announcements.iter().any(|a| &a.target == p))
            .collect();
        let total = self.announcements.len() + active.len();
        let room = MAX_LINES.saturating_sub(self.announcements.len().min(MAX_LINES));
        let shown: Vec<AgentTarget> = active.iter().take(room).cloned().collect();

        // Keep the visible agents' activity fresh.
        if self.activity_read.is_none_or(|t| now.duration_since(t) >= ACTIVITY_EVERY) {
            self.activity_read = Some(now);
            for path in &shown {
                let t = &self.agents[path];
                if t.state == AgentState::Running && t.kind == AgentKind::ClaudeCode {
                    match activity::current(t.tool.as_ref(), t.session_file.as_deref()) {
                        Some(a) => self.activity.insert(path.clone(), a),
                        None => self.activity.remove(path),
                    };
                }
            }
        }

        let bowing = self.bow_started.and_then(|s| {
            let f = (now.duration_since(s).as_millis() / BOW_FRAME.as_millis()) as usize;
            (f < BOW_FRAMES).then_some(f)
        });
        if bowing.is_none() {
            self.bow_started = None;
        }
        // One conductor for the whole orchestra. While anyone plays he keeps
        // the beat — the music doesn't stop because one player needs you —
        // and raises his free hand to cue you if someone's waiting. The
        // baton only stops, held up, when the waiting is all that's left.
        let running = self.agents.values().filter(|t| t.state == AgentState::Running).count();
        let any_waiting = self.agents.iter().any(|(p, t)| self.waiting(p, t));
        let beat = (now.duration_since(self.clock).as_millis() / BEAT_FRAME.as_millis()) as usize % CONDUCT_FRAMES;
        let pose = match (bowing, running > 0, any_waiting) {
            (Some(f), _, _) => Pose::Bow(f),
            (None, true, true) => Pose::ConductCue(beat),
            (None, true, false) => Pose::Conduct(beat),
            (None, false, true) => Pose::Cue,
            (None, false, false) => Pose::Rest,
        };

        let mut lines: Vec<Caption> = self
            .announcements
            .iter()
            .take(MAX_LINES)
            .map(|a| Caption {
                label: a.label.clone(),
                text: a.text.clone(),
                nav: a.nav.clone(),
                announcement: true,
                urgent: true,
            })
            .collect();
        for path in &shown {
            let t = &self.agents[path];
            lines.push(Caption {
                label: t.label.clone(),
                text: self.status_text(path, now),
                nav: nav_for(path, &t.kind),
                announcement: false,
                urgent: t.state == AgentState::Waiting,
            });
        }

        View {
            frame: scene::frame_index(pose, eyes_open || matches!(pose, Pose::Bow(_))),
            more: total.saturating_sub(lines.len()),
            running,
            lines,
        }
    }

    /// How soon the next tick matters: fast while there's motion, slower at
    /// rest where only blinks and the seconds counter change.
    pub fn tick_interval(&self) -> Duration {
        let moving = self.bow_started.is_some()
            || self.agents.values().any(|t| t.state == AgentState::Running);
        if moving { BEAT_FRAME } else { Duration::from_millis(100) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(root: &str) -> AgentTarget {
        AgentTarget::new(Path::new(root), None)
    }

    fn conductor_with(root: &str, state: AgentState, now: Instant) -> Conductor {
        let mut c = Conductor::default();
        c.observe(
            &target(root),
            AgentKind::Codex,
            state,
            WaitReason::None,
            None,
            None,
            "(app)".into(),
            now,
        );
        c
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(format_elapsed(Duration::from_secs(83)), "1m 23s");
        assert_eq!(format_elapsed(Duration::from_secs(7500)), "2h 05m");
    }

    #[test]
    fn a_running_agent_conducts_and_counts() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Running, t0);
        let v = c.tick(t0 + Duration::from_secs(83), None, |_, _| String::new());
        let cap = v.lines[0].clone();
        assert_eq!(cap.label, "(app)");
        assert_eq!(cap.text, "Working for 1m 23s…");
        assert_eq!(v.running, 1);
        assert!(v.frame < CONDUCT_FRAMES * 2, "conducting frames come first");
    }

    #[test]
    fn sibling_tabs_count_and_finish_independently() {
        let now = Instant::now();
        let root = Path::new("/worktree");
        let first = AgentTarget::new(root, Some(1));
        let second = AgentTarget::new(root, Some(2));
        let mut conductor = Conductor::default();
        let observe = |conductor: &mut Conductor, target, state, reason| {
            conductor.observe(target, AgentKind::Codex, state, reason, None, None, "(app)".into(), now)
        };
        for tab in [&first, &second] {
            observe(&mut conductor, tab, AgentState::Running, WaitReason::None);
        }
        let view = conductor.tick(now, Some(&second), |tab, _| tab.tab_id.unwrap().to_string());
        assert_eq!(view.running, 2);
        assert_eq!(view.lines.iter().map(|line| line.nav.as_str()).collect::<Vec<_>>(), ["2", "1"]);

        assert_eq!(observe(&mut conductor, &first, AgentState::Idle, WaitReason::TurnComplete), Some(AgentState::Running));
        assert_eq!(conductor.root_state(root), AgentState::Running);
        assert_eq!(conductor.tick(now, None, |_, _| String::new()).running, 1);
        conductor.stopped(&second);
        assert_eq!(conductor.root_state(root), AgentState::Idle);
        assert_eq!(conductor.tick(now, None, |_, _| String::new()).running, 0);
    }

    #[test]
    fn idle_rests_with_no_caption() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Idle, t0);
        let v = c.tick(t0, None, |_, _| String::new());
        assert!(v.lines.is_empty());
        assert_eq!(v.frame, scene::frame_index(Pose::Rest, true));
    }

    #[test]
    fn the_clock_survives_refreshes_and_resets_after_idle() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Running, t0);
        let again = |c: &mut Conductor, s, at| {
            c.observe(&target("/w"), AgentKind::Codex, s, WaitReason::None, None, None, "(app)".into(), at)
        };
        again(&mut c, AgentState::Running, t0 + Duration::from_secs(30));
        let v = c.tick(t0 + Duration::from_secs(40), None, |_, _| String::new());
        assert_eq!(v.lines[0].clone().text, "Working for 40s…");
        again(&mut c, AgentState::Idle, t0 + Duration::from_secs(41));
        again(&mut c, AgentState::Running, t0 + Duration::from_secs(50));
        let v = c.tick(t0 + Duration::from_secs(55), None, |_, _| String::new());
        assert_eq!(v.lines[0].clone().text, "Working for 5s…");
    }

    #[test]
    fn a_waiting_agent_gets_the_cue_pose() {
        let t0 = Instant::now();
        let mut c = Conductor::default();
        c.observe(
            &target("/w"),
            AgentKind::ClaudeCode,
            AgentState::Waiting,
            WaitReason::PermissionPrompt,
            None,
            None,
            "(app)".into(),
            t0,
        );
        let v = c.tick(t0, None, |_, _| String::new());
        assert_eq!(v.lines[0].clone().text, "Needs your OK");
        assert_eq!(v.frame, scene::frame_index(Pose::Cue, true));
    }

    #[test]
    fn up_to_three_agents_stack_selected_first_then_more() {
        let t0 = Instant::now();
        let mut c = Conductor::default();
        for (root, label) in [("/a", "(a)"), ("/b", "(b)"), ("/c", "(c)"), ("/d", "(d)"), ("/e", "(e)")] {
            c.observe(&target(root), AgentKind::Codex, AgentState::Running, WaitReason::None, None, None, label.into(), t0);
        }
        c.observe(&target("/d"), AgentKind::Codex, AgentState::Waiting, WaitReason::PermissionPrompt, None, None, "(d)".into(), t0);
        let v = c.tick(t0, Some(&target("/c")), |p, _| p.root.display().to_string());
        let labels: Vec<&str> = v.lines.iter().map(|l| l.label.as_str()).collect();
        // Selected, then waiting on you, then running by path.
        assert_eq!(labels, ["(c)", "(d)", "(a)"]);
        assert_eq!(v.more, 2);
        assert_eq!(v.lines[1].text, "Needs your OK");
        assert_eq!(v.lines[0].nav, "/c", "each line navigates to its own agent");
    }

    #[test]
    fn he_keeps_the_beat_and_cues_while_one_waits() {
        let t0 = Instant::now();
        let mut c = conductor_with("/a", AgentState::Running, t0);
        let v = c.tick(t0, None, |_, _| String::new());
        assert!(v.frame < CONDUCT_FRAMES * 2, "conducts while anyone plays");

        c.observe(&target("/b"), AgentKind::Codex, AgentState::Waiting, WaitReason::None, None, None, "(b)".into(), t0);
        let v = c.tick(t0, None, |_, _| String::new());
        let cueing = |f| (0..CONDUCT_FRAMES).any(|b| [true, false].iter().any(|&e| scene::frame_index(Pose::ConductCue(b), e) == f));
        assert!(cueing(v.frame), "still beating, free hand raised");

        // Only the waiting one left: baton up and still.
        c.stopped(&target("/a"));
        let v = c.tick(t0, None, |_, _| String::new());
        assert!(v.frame == scene::frame_index(Pose::Cue, true) || v.frame == scene::frame_index(Pose::Cue, false));
        assert_eq!(v.lines.len(), 1, "a stopped agent leaves the bubble");
    }

    #[test]
    fn an_announcement_takes_its_agents_line() {
        let t0 = Instant::now();
        let mut c = conductor_with("/a", AgentState::Running, t0);
        c.observe(&target("/b"), AgentKind::Codex, AgentState::Idle, WaitReason::TurnComplete, None, None, "(b)".into(), t0);
        c.announce(&target("/b"), "(b)".into(), "Codex finished — your turn".into(), String::new(), true, t0);
        let v = c.tick(t0, None, |_, _| String::new());
        let texts: Vec<(&str, bool)> = v.lines.iter().map(|l| (l.label.as_str(), l.announcement)).collect();
        assert_eq!(texts, [("(b)", true), ("(app)", false)]);
        // Starting work again retires the stale "finished".
        c.observe(&target("/b"), AgentKind::Codex, AgentState::Running, WaitReason::None, None, None, "(b)".into(), t0);
        let v = c.tick(t0, None, |_, _| String::new());
        assert!(v.lines.iter().all(|l| !l.announcement));
        assert_eq!(v.lines.len(), 2);
    }

    fn wait_on(c: &mut Conductor, root: &str, state: AgentState, at: Instant) {
        c.observe(&target(root), AgentKind::ClaudeCode, state, WaitReason::PermissionPrompt, None, None, "(w)".into(), at);
    }

    #[test]
    fn going_to_a_waiting_agent_dismisses_it_until_it_waits_again() {
        let t0 = Instant::now();
        let mut c = Conductor::default();
        wait_on(&mut c, "/w", AgentState::Waiting, t0);
        let v = c.tick(t0, Some(&target("/other")), |_, _| String::new());
        assert_eq!(v.lines[0].text, "Needs your OK");

        // You go and look: the line goes, and so does his cue.
        let v = c.tick(t0, Some(&target("/w")), |_, _| String::new());
        assert!(v.lines.is_empty());
        assert!(v.frame == scene::frame_index(Pose::Rest, true) || v.frame == scene::frame_index(Pose::Rest, false));

        // The same wait refreshing stays dismissed, even after you leave.
        wait_on(&mut c, "/w", AgentState::Waiting, t0);
        let v = c.tick(t0, Some(&target("/other")), |_, _| String::new());
        assert!(v.lines.is_empty());

        // A new wait after it moves on comes back.
        wait_on(&mut c, "/w", AgentState::Running, t0);
        wait_on(&mut c, "/w", AgentState::Waiting, t0);
        let v = c.tick(t0, Some(&target("/other")), |_, _| String::new());
        assert_eq!(v.lines[0].text, "Needs your OK");
    }

    #[test]
    fn a_wait_that_starts_while_you_are_there_still_shows() {
        let t0 = Instant::now();
        let mut c = Conductor::default();
        wait_on(&mut c, "/w", AgentState::Running, t0);
        c.tick(t0, Some(&target("/w")), |_, _| String::new());
        wait_on(&mut c, "/w", AgentState::Waiting, t0);
        let v = c.tick(t0, Some(&target("/w")), |_, _| String::new());
        assert_eq!(v.lines[0].text, "Needs your OK");
        // Clicking it (dismiss) clears it in place.
        c.dismiss(&target("/w"));
        let v = c.tick(t0, Some(&target("/w")), |_, _| String::new());
        assert!(v.lines.is_empty());
    }

    #[test]
    fn going_to_a_finished_agent_retires_its_announcement() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Idle, t0);
        c.announce(&target("/w"), "(app)".into(), "Codex finished — your turn".into(), String::new(), true, t0);
        let v = c.tick(t0, Some(&target("/other")), |_, _| String::new());
        assert_eq!(v.lines.len(), 1);
        let v = c.tick(t0, Some(&target("/w")), |_, _| String::new());
        assert!(v.lines.is_empty());
    }

    #[test]
    fn only_waits_and_announcements_need_you() {
        let t0 = Instant::now();
        let mut c = conductor_with("/run", AgentState::Running, t0);
        assert!(!c.tick(t0, None, |_, _| String::new()).needs_you(), "progress alone is not urgent");
        wait_on(&mut c, "/w", AgentState::Waiting, t0);
        assert!(c.tick(t0, None, |_, _| String::new()).needs_you());
        c.dismiss(&target("/w"));
        assert!(!c.tick(t0, None, |_, _| String::new()).needs_you(), "a seen wait no longer is");
        c.announce(&target("/run"), "(app)".into(), "finished".into(), String::new(), true, t0);
        assert!(c.tick(t0, None, |_, _| String::new()).needs_you());
    }

    #[test]
    fn a_finish_bows_then_rests() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Idle, t0);
        c.announce(&target("/w"), "(app)".into(), "Codex finished — your turn".into(), String::new(), true, t0);
        let v = c.tick(t0, None, |_, _| String::new());
        assert_eq!(v.frame, scene::frame_index(Pose::Bow(0), false));
        assert_eq!(v.lines[0].clone().text, "Codex finished — your turn");
        let v = c.tick(t0 + BOW_FRAME * (BOW_FRAMES as u32 + 1), None, |_, _| String::new());
        assert!(matches!(v.frame, f if f == scene::frame_index(Pose::Rest, true) || f == scene::frame_index(Pose::Rest, false)));
        let v = c.tick(t0 + ANNOUNCE_FOR, None, |_, _| String::new());
        assert!(v.lines.is_empty());
    }

    #[test]
    fn he_blinks() {
        let t0 = Instant::now();
        let mut c = conductor_with("/w", AgentState::Idle, t0);
        let mut closed = 0;
        for i in 0..200 {
            let v = c.tick(t0 + Duration::from_millis(i * 50), None, |_, _| String::new());
            if v.frame == scene::frame_index(Pose::Rest, false) {
                closed += 1;
            }
        }
        assert!(closed > 0, "eyes close at least once in 10 s");
        assert!(closed < 40, "but are mostly open ({closed}/200)");
    }
}
