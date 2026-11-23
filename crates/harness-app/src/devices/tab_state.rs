//! The Device tab's pure state machine: what the tab shows and which capture
//! work should be running, derived from simulator lists and stream events.
//! It never does I/O; callers run the returned [`Command`]s.

use super::simctl::ListError;
use super::{choose_device, DeviceInfo, StreamError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeviceView {
    NeedsXcode,
    NoneRunning,
    Live { udid: String },
    /// Screenshots instead of the live view. `transient`: the live view
    /// failed for a reason that may pass, and is tried again; otherwise
    /// this Xcode's display API isn't supported.
    Reduced { udid: String, why: String, transient: bool },
    ShutDown { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    StartStream(String),
    StopStream,
    StartScreenshots(String),
    StopScreenshots,
}

/// What is currently running, as far as commands issued so far go.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Running {
    Nothing,
    Stream(String),
    Screenshots(String),
}

/// Consecutive transient failures of one device's live view that are retried
/// on later lists, after 1, 2, 4 and then 8 good lists (about 3 to 24 s).
/// Past that only a show or another device tries live again.
const LIST_RETRIES: u32 = 4;

/// The live view of `udid` failed for a reason that may pass.
struct Retry {
    udid: String,
    /// In a row, since the last live view that started.
    failures: u32,
    /// Good lists until live is tried again; 0 once list retries ran out.
    lists_left: u32,
}

pub(crate) struct DeviceTab {
    pub booted: Vec<DeviceInfo>,
    /// The device the user picked (saved in prefs): watched whenever it is
    /// booted.
    pub preferred: Option<String>,
    /// The device being watched (or last watched).
    pub selected: Option<String>,
    pub view: DeviceView,
    hidden: bool,
    running: Running,
    /// Set once this Xcode's display API turned out unsupported; later
    /// devices go straight to screenshots because that is about Xcode, not a
    /// device. Other failures only fall back for one start ([`Retry`]).
    reduced_why: Option<String>,
    retry: Option<Retry>,
    /// A device reported gone, and the last list ticket handed out by then:
    /// a list with that ticket or older was already under way, so it may
    /// still show the device booted. A newer list showing it means it
    /// booted again.
    gone: Option<(String, u64)>,
    /// The last ticket [`Self::list_started`] handed out.
    lists_started: u64,
}

impl DeviceTab {
    pub(crate) fn new() -> Self {
        Self {
            booted: Vec::new(),
            preferred: None,
            selected: None,
            view: DeviceView::NoneRunning,
            hidden: false,
            running: Running::Nothing,
            reduced_why: None,
            retry: None,
            gone: None,
            lists_started: 0,
        }
    }

    /// A device list is being fetched: its ticket, for [`Self::on_list`].
    pub(crate) fn list_started(&mut self) -> u64 {
        self.lists_started += 1;
        self.lists_started
    }

    fn view_for(&self, udid: &str) -> DeviceView {
        match &self.reduced_why {
            Some(why) => DeviceView::Reduced {
                udid: udid.into(),
                why: why.clone(),
                transient: false,
            },
            None => DeviceView::Live { udid: udid.into() },
        }
    }

    fn shows(&self, udid: &str) -> bool {
        matches!(&self.view,
            DeviceView::Live { udid: u } | DeviceView::Reduced { udid: u, .. } if u == udid)
    }

    /// Diff what should run against what runs, and return the commands.
    fn reconcile(&mut self) -> Vec<Command> {
        let want = if self.hidden {
            Running::Nothing
        } else {
            match &self.view {
                DeviceView::Live { udid } => Running::Stream(udid.clone()),
                DeviceView::Reduced { udid, .. } => Running::Screenshots(udid.clone()),
                _ => Running::Nothing,
            }
        };
        if want == self.running {
            return Vec::new();
        }
        let mut cmds = Vec::new();
        match &self.running {
            Running::Stream(_) => cmds.push(Command::StopStream),
            Running::Screenshots(_) => cmds.push(Command::StopScreenshots),
            Running::Nothing => {}
        }
        match &want {
            Running::Stream(u) => cmds.push(Command::StartStream(u.clone())),
            Running::Screenshots(u) => cmds.push(Command::StartScreenshots(u.clone())),
            Running::Nothing => {}
        }
        self.running = want;
        cmds
    }

    /// A list fetched under `ticket` ([`Self::list_started`]) landed.
    pub(crate) fn on_list(
        &mut self,
        ticket: u64,
        list: Result<Vec<DeviceInfo>, ListError>,
    ) -> Vec<Command> {
        match list {
            // The pick (`preferred`) stays, for when Xcode is back.
            Err(ListError::NoXcode) => {
                self.booted.clear();
                self.selected = None;
                self.gone = None;
                self.view = DeviceView::NeedsXcode;
            }
            // A transient hiccup (a slow simctl included) must not tear down
            // a working stream.
            Err(ListError::Failed(_)) => return Vec::new(),
            Ok(list) => {
                self.booted = list;
                if let Some((g, at)) = &self.gone
                    && (ticket > *at || !self.booted.iter().any(|d| &d.udid == g))
                {
                    self.gone = None;
                }
                let chosen = choose_device(
                    &self.booted,
                    self.preferred.as_deref(),
                    self.selected.as_deref(),
                )
                .map(|d| d.udid.clone());
                self.selected = chosen.clone();
                match chosen {
                    None => self.view = DeviceView::NoneRunning,
                    Some(udid) if self.gone.as_ref().is_some_and(|(g, _)| *g == udid) => {}
                    Some(udid) if self.shows(&udid) => self.count_down_retry(),
                    Some(udid) => self.view = self.view_for(&udid),
                }
            }
        }
        self.reconcile()
    }

    pub(crate) fn on_select(&mut self, udid: &str) -> Vec<Command> {
        if !self.booted.iter().any(|d| d.udid == udid) {
            return Vec::new();
        }
        self.preferred = Some(udid.into());
        self.selected = Some(udid.into());
        self.gone = None;
        if !self.shows(udid) {
            self.view = self.view_for(udid);
        }
        self.reconcile()
    }

    /// The live view couldn't start: screenshots instead. For good when
    /// the display API is unsupported; otherwise for this start only (see
    /// [`Retry`]).
    pub(crate) fn on_stream_failed(&mut self, err: StreamError) -> Vec<Command> {
        let (why, transient) = match err {
            StreamError::DeviceNotFound => return self.on_gone(),
            StreamError::NoXcode => ("Xcode not found".to_string(), true),
            StreamError::Unsupported(s) => (s.to_string(), false),
            StreamError::Failed(s) => (s, true),
        };
        let DeviceView::Live { udid } = &self.view else {
            return Vec::new();
        };
        let udid = udid.clone();
        if transient {
            let failures = match &self.retry {
                Some(r) if r.udid == udid => r.failures + 1,
                _ => 1,
            };
            let lists_left = if failures <= LIST_RETRIES { 1 << (failures - 1) } else { 0 };
            self.retry = Some(Retry { udid: udid.clone(), failures, lists_left });
        } else {
            self.reduced_why = Some(why.clone());
        }
        self.view = DeviceView::Reduced { udid, why, transient };
        self.reconcile()
    }

    /// Another good list while a transient fallback shows: live again once
    /// its wait is over.
    fn count_down_retry(&mut self) {
        let DeviceView::Reduced { udid, transient: true, .. } = &self.view else {
            return;
        };
        let Some(retry) = self.retry.as_mut().filter(|r| &r.udid == udid && r.lists_left > 0)
        else {
            return;
        };
        retry.lists_left -= 1;
        if retry.lists_left == 0 {
            self.view = DeviceView::Live { udid: udid.clone() };
        }
    }

    pub(crate) fn on_gone(&mut self) -> Vec<Command> {
        let (DeviceView::Live { udid } | DeviceView::Reduced { udid, .. }) = &self.view else {
            return Vec::new();
        };
        let name = self
            .booted
            .iter()
            .find(|d| &d.udid == udid)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| udid.clone());
        self.gone = Some((udid.clone(), self.lists_started));
        self.view = DeviceView::ShutDown { name };
        self.reconcile()
    }

    /// The live view started: a later failure starts its backoff afresh.
    // Elsewhere than macOS no stream starts.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn on_stream_started(&mut self) {
        self.retry = None;
    }

    pub(crate) fn on_hidden(&mut self) -> Vec<Command> {
        self.hidden = true;
        self.reconcile()
    }

    /// Back on screen. A transient fallback tries live again.
    pub(crate) fn on_shown(&mut self) -> Vec<Command> {
        self.hidden = false;
        if let DeviceView::Reduced { udid, transient: true, .. } = &self.view {
            self.view = DeviceView::Live { udid: udid.clone() };
        }
        self.reconcile()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: format!("name-{udid}"),
            os: "iOS 27.2".into(),
            device_type: "iPhone-18-Pro".into(),
        }
    }

    fn live(udid: &str) -> DeviceView {
        DeviceView::Live { udid: udid.into() }
    }

    /// A list started now, landing now.
    fn fresh(t: &mut DeviceTab, list: Result<Vec<DeviceInfo>, ListError>) -> Vec<Command> {
        let ticket = t.list_started();
        t.on_list(ticket, list)
    }

    fn tab_on(udids: &[&str]) -> DeviceTab {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(udids.iter().map(|u| dev(u)).collect()));
        t
    }

    #[test]
    fn first_list_starts_a_stream_on_the_first_device() {
        let mut t = DeviceTab::new();
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
        assert_eq!(t.view, live("a"));
        assert_eq!(t.selected.as_deref(), Some("a"));
    }

    #[test]
    fn no_xcode_shows_needs_xcode_and_starts_nothing() {
        let mut t = DeviceTab::new();
        assert!(fresh(&mut t, Err(ListError::NoXcode)).is_empty());
        assert_eq!(t.view, DeviceView::NeedsXcode);
    }

    #[test]
    fn no_xcode_stops_what_runs() {
        let mut t = tab_on(&["a"]);
        assert_eq!(fresh(&mut t, Err(ListError::NoXcode)), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::NeedsXcode);
    }

    #[test]
    fn an_empty_list_shows_none_running() {
        let mut t = DeviceTab::new();
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn a_failed_list_keeps_the_current_view() {
        let mut t = tab_on(&["a"]);
        assert!(fresh(&mut t, Err(ListError::Failed("timeout".into()))).is_empty());
        assert_eq!(t.view, live("a"));
        assert_eq!(t.booted.len(), 1);
    }

    #[test]
    fn a_stream_failure_falls_back_to_screenshots() {
        let mut t = tab_on(&["a"]);
        let cmds = t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(cmds, vec![Command::StopStream, Command::StartScreenshots("a".into())]);
        assert_eq!(
            t.view,
            DeviceView::Reduced {
                udid: "a".into(),
                why: "SimDeviceScreen".into(),
                transient: false
            }
        );
    }

    #[test]
    fn no_xcode_stream_failure_also_falls_back_to_screenshots() {
        let mut t = tab_on(&["a"]);
        let cmds = t.on_stream_failed(StreamError::NoXcode);
        assert_eq!(cmds, vec![Command::StopStream, Command::StartScreenshots("a".into())]);
        assert!(matches!(t.view, DeviceView::Reduced { .. }));
    }

    #[test]
    fn device_not_found_behaves_like_gone() {
        let mut t = tab_on(&["a"]);
        assert_eq!(t.on_stream_failed(StreamError::DeviceNotFound), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-a".into() });
    }

    #[test]
    fn the_device_going_away_shows_shut_down_and_stops() {
        let mut t = tab_on(&["a"]);
        assert_eq!(t.on_gone(), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-a".into() });
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn a_stale_list_does_not_revive_a_gone_device() {
        let mut t = tab_on(&["a"]);
        // Fetched before the device went, so it still shows it booted.
        let stale = t.list_started();
        t.on_gone();
        assert!(t.on_list(stale, Ok(vec![dev("a")])).is_empty());
        assert!(matches!(t.view, DeviceView::ShutDown { .. }));
        fresh(&mut t, Ok(vec![]));
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
    }

    #[test]
    fn a_vanished_selection_moves_to_the_next_booted_device() {
        let mut t = tab_on(&["a"]);
        assert_eq!(
            fresh(&mut t, Ok(vec![dev("b")])),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
        assert_eq!(t.view, live("b"));
        assert_eq!(t.selected.as_deref(), Some("b"));
    }

    #[test]
    fn the_same_list_again_issues_no_commands() {
        let mut t = tab_on(&["a"]);
        assert!(fresh(&mut t, Ok(vec![dev("a")])).is_empty());
        t.on_hidden();
        assert!(fresh(&mut t, Ok(vec![dev("a")])).is_empty());
        assert!(fresh(&mut t, Ok(vec![dev("a")])).is_empty());
    }

    #[test]
    fn hiding_stops_everything_and_showing_resumes_the_selection() {
        let mut t = tab_on(&["a", "b"]);
        t.on_select("b");
        assert_eq!(t.on_hidden(), vec![Command::StopStream]);
        assert!(t.on_hidden().is_empty());
        assert_eq!(t.on_shown(), vec![Command::StartStream("b".into())]);
    }

    #[test]
    fn hidden_events_update_state_but_start_nothing() {
        let mut t = tab_on(&["a"]);
        t.on_hidden();
        assert!(fresh(&mut t, Ok(vec![dev("b")])).is_empty());
        assert_eq!(t.view, live("b"));
        assert_eq!(t.on_shown(), vec![Command::StartStream("b".into())]);
    }

    #[test]
    fn showing_resumes_screenshots_when_reduced() {
        let mut t = tab_on(&["a"]);
        t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(t.on_hidden(), vec![Command::StopScreenshots]);
        assert_eq!(t.on_shown(), vec![Command::StartScreenshots("a".into())]);
    }

    #[test]
    fn selecting_another_device_switches_the_stream() {
        let mut t = tab_on(&["a", "b"]);
        assert_eq!(
            t.on_select("b"),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
        assert_eq!(t.view, live("b"));
        assert!(t.on_select("b").is_empty());
        assert!(t.on_select("zzz").is_empty());
    }

    #[test]
    fn once_reduced_a_new_device_also_uses_screenshots() {
        let mut t = tab_on(&["a", "b"]);
        t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(
            t.on_select("b"),
            vec![Command::StopScreenshots, Command::StartScreenshots("b".into())]
        );
        assert!(matches!(&t.view, DeviceView::Reduced { udid, .. } if udid == "b"));
    }

    fn reduced(udid: &str, why: &str, transient: bool) -> DeviceView {
        DeviceView::Reduced { udid: udid.into(), why: why.into(), transient }
    }

    #[test]
    fn the_picked_device_is_watched_again_when_it_boots_again() {
        let mut t = tab_on(&["a", "b"]);
        t.on_select("b");
        // b shuts down: the tab moves on to a.
        assert_eq!(t.on_gone(), vec![Command::StopStream]);
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
        assert_eq!(t.selected.as_deref(), Some("a"));
        // b boots again: back to it.
        assert_eq!(
            fresh(&mut t, Ok(vec![dev("a"), dev("b")])),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
        assert_eq!(t.view, live("b"));
        assert_eq!(t.preferred.as_deref(), Some("b"));
    }

    #[test]
    fn the_saved_pick_is_watched_when_it_boots_after_the_first_list() {
        let mut t = DeviceTab::new();
        t.preferred = Some("b".into());
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
        assert_eq!(
            fresh(&mut t, Ok(vec![dev("a"), dev("b")])),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
    }

    #[test]
    fn needs_xcode_then_live_keeps_the_pick() {
        let mut t = tab_on(&["a", "b"]);
        t.on_select("b");
        fresh(&mut t, Err(ListError::NoXcode));
        assert_eq!(t.view, DeviceView::NeedsXcode);
        assert_eq!(
            fresh(&mut t, Ok(vec![dev("a"), dev("b")])),
            vec![Command::StartStream("b".into())]
        );
        assert_eq!(t.view, live("b"));
    }

    #[test]
    fn an_empty_list_keeps_the_pick() {
        let mut t = tab_on(&["a", "b"]);
        t.on_select("b");
        fresh(&mut t, Ok(vec![]));
        assert_eq!(t.view, DeviceView::NoneRunning);
        assert_eq!(
            fresh(&mut t, Ok(vec![dev("a"), dev("b")])),
            vec![Command::StartStream("b".into())]
        );
    }

    #[test]
    fn without_its_pick_booted_the_tab_stays_on_the_device_it_watches() {
        // No pick: a device booting with an earlier name doesn't take over.
        let mut t = tab_on(&["b"]);
        assert!(fresh(&mut t, Ok(vec![dev("a"), dev("b")])).is_empty());
        assert_eq!(t.view, live("b"));
        // A pick that isn't booted: the same.
        let mut t = tab_on(&["c", "p"]);
        t.on_select("p");
        fresh(&mut t, Ok(vec![dev("c")]));
        assert_eq!(t.view, live("c"));
        assert!(fresh(&mut t, Ok(vec![dev("a"), dev("c")])).is_empty());
        assert_eq!(t.view, live("c"));
    }

    #[test]
    fn a_device_restarted_between_polls_is_watched_again() {
        // `simctl shutdown a && simctl boot a` inside one poll gap: the list
        // started after the Gone still has a, so a is back.
        let mut t = tab_on(&["a"]);
        t.on_gone();
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
        assert_eq!(t.view, live("a"));
    }

    #[test]
    fn a_transient_stream_failure_falls_back_for_this_start_only() {
        let mut t = tab_on(&["a"]);
        assert_eq!(
            t.on_stream_failed(StreamError::Failed("the device has no main display".into())),
            vec![Command::StopStream, Command::StartScreenshots("a".into())]
        );
        assert_eq!(t.view, reduced("a", "the device has no main display", true));
        // The next show tries live again.
        assert_eq!(t.on_hidden(), vec![Command::StopScreenshots]);
        assert_eq!(t.on_shown(), vec![Command::StartStream("a".into())]);
        assert_eq!(t.view, live("a"));
    }

    #[test]
    fn after_a_transient_failure_another_device_goes_live() {
        let mut t = tab_on(&["a", "b"]);
        t.on_stream_failed(StreamError::NoXcode);
        assert!(matches!(&t.view, DeviceView::Reduced { transient: true, .. }));
        assert_eq!(
            t.on_select("b"),
            vec![Command::StopScreenshots, Command::StartStream("b".into())]
        );
    }

    /// How many lists after each failure the tab tries live again (None:
    /// not within 100).
    fn lists_until_retry(t: &mut DeviceTab) -> Option<usize> {
        t.on_stream_failed(StreamError::Failed("x".into()));
        (1..=100).find(|_| !fresh(t, Ok(vec![dev("a")])).is_empty())
    }

    #[test]
    fn a_transient_failure_retries_on_later_lists_with_a_bounded_backoff() {
        let mut t = tab_on(&["a"]);
        let waits: Vec<_> = (0..6).map(|_| lists_until_retry(&mut t)).collect();
        assert_eq!(waits, vec![Some(1), Some(2), Some(4), Some(8), None, None]);
        // Still the next show tries again.
        t.on_hidden();
        assert_eq!(t.on_shown(), vec![Command::StartStream("a".into())]);
        // A live view that started resets the backoff.
        t.on_stream_started();
        assert_eq!(lists_until_retry(&mut t), Some(1));
    }

    #[test]
    fn an_unsupported_display_api_never_retries_live() {
        let mut t = tab_on(&["a"]);
        t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(t.view, reduced("a", "SimDeviceScreen", false));
        for _ in 0..20 {
            assert!(fresh(&mut t, Ok(vec![dev("a")])).is_empty());
        }
    }
}
