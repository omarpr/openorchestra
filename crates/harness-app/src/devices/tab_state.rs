//! The Device tab's pure state machine: what the tab shows and which capture
//! work should be running, derived from simulator lists and stream events.
//! It never does I/O; callers run the returned [`Command`]s.

use super::simctl::ListError;
use super::{AndroidRunning, DeviceInfo, Platform, StreamError, choose_device};

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
    /// A running Android emulator, streamed. `why`: its stream ended or
    /// couldn't start, said in place of the picture; it is tried again
    /// after a few lists (as a transient [`Self::Reduced`] is), never
    /// reduced to screenshots. `starting`: that was an emulator still
    /// booting, so the panel says so instead, and it's tried again on the
    /// next list without using up the retries.
    Android { udid: String, why: Option<String>, starting: bool },
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
    Stream(String, Option<AndroidRunning>),
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
    /// Something covers the picture (a boot offer in its place) though the
    /// tab is on screen: nothing streams, but lists still land.
    obscured: bool,
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
            obscured: false,
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
        // Nothing of the simulator path (simctl, its stream or screenshots)
        // is for an emulator, and why this Xcode reduced simulators is no
        // reason to reduce it.
        if Platform::of_id(udid) == Platform::Android {
            return DeviceView::Android { udid: udid.into(), why: None, starting: false };
        }
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
            DeviceView::Live { udid: u }
            | DeviceView::Reduced { udid: u, .. }
            | DeviceView::Android { udid: u, .. } if u == udid)
    }

    fn listed_android<'a>(list: &'a [DeviceInfo], udid: &str) -> Option<&'a AndroidRunning> {
        list.iter().find(|d| d.udid == udid).and_then(|d| d.android.as_ref())
    }

    /// The emulator bound to the current stream, independent of the saved AVD ID.
    pub(crate) fn android_stream_target(&self) -> Option<&AndroidRunning> {
        match &self.running {
            Running::Stream(_, dev) => dev.as_ref(),
            _ => None,
        }
    }

    /// Diff what should run against what runs, and return the commands.
    fn reconcile(&mut self) -> Vec<Command> {
        let want = if self.hidden || self.obscured {
            Running::Nothing
        } else {
            match &self.view {
                // An emulator's stream too: the panel sends an Android id to
                // the emulator, never to simctl.
                DeviceView::Live { udid } | DeviceView::Android { udid, why: None, .. } => {
                    Running::Stream(udid.clone(), Self::listed_android(&self.booted, udid).cloned())
                }
                DeviceView::Reduced { udid, .. } => Running::Screenshots(udid.clone()),
                _ => Running::Nothing,
            }
        };
        if want == self.running {
            return Vec::new();
        }
        let mut cmds = Vec::new();
        match &self.running {
            Running::Stream(..) => cmds.push(Command::StopStream),
            Running::Screenshots(_) => cmds.push(Command::StopScreenshots),
            Running::Nothing => {}
        }
        match &want {
            Running::Stream(u, _) => cmds.push(Command::StartStream(u.clone())),
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
        self.apply_list(ticket, list, true)
    }

    fn apply_list(
        &mut self,
        ticket: u64,
        list: Result<Vec<DeviceInfo>, ListError>,
        ios_poll_succeeded: bool,
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
                // A watched emulator that left the list has exited: it shut
                // down, as a simulator's stream says when it goes. Its own
                // stream may not have seen it: one saving its snapshot runs
                // on for seconds after ending the call.
                let left = match &self.view {
                    DeviceView::Android { udid, .. } if !list.iter().any(|d| &d.udid == udid) => {
                        Some(udid.clone())
                    }
                    _ => None,
                };
                let left = left.map(|udid| (self.name_of(&udid), udid));
                let replaced = match &self.view {
                    DeviceView::Android { udid, .. } => {
                        let before = Self::listed_android(&self.booted, udid);
                        let after = Self::listed_android(&list, udid);
                        after.is_some() && before != after
                    }
                    _ => false,
                };
                self.booted = list;
                if let Some((g, at)) = &self.gone
                    && (ticket > *at || !self.booted.iter().any(|d| &d.udid == g))
                {
                    self.gone = None;
                }
                if let Some((name, udid)) = left {
                    self.gone = Some((udid, self.lists_started));
                    self.view = DeviceView::ShutDown { name };
                    return self.reconcile();
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
                    Some(udid) if replaced && self.shows(&udid) => {
                        self.retry = None;
                        self.view = self.view_for(&udid);
                    }
                    Some(udid) if self.shows(&udid) => {
                        if ios_poll_succeeded || Platform::of_id(&udid) == Platform::Android {
                            self.count_down_retry();
                        }
                    }
                    Some(udid) => self.view = self.view_for(&udid),
                }
            }
        }
        self.reconcile()
    }

    /// A list fetched under `ticket` landed in two parts: the simulators
    /// (`ios`, which may have failed) and the running Android emulators,
    /// which count whatever simctl said.
    pub(crate) fn on_lists(
        &mut self,
        ticket: u64,
        ios: Result<Vec<DeviceInfo>, ListError>,
        android: Vec<DeviceInfo>,
    ) -> Vec<Command> {
        match ios {
            Ok(mut list) => {
                list.extend(android);
                self.on_list(ticket, Ok(list))
            }
            // A watched emulator that just left goes by way of "shut down".
            Err(ListError::NoXcode)
                if android.is_empty() && !matches!(self.view, DeviceView::Android { .. }) =>
            {
                self.on_list(ticket, Err(ListError::NoXcode))
            }
            // Android Studio without Xcode: the emulators alone.
            Err(ListError::NoXcode) => self.on_list(ticket, Ok(android)),
            Err(ListError::Failed(e)) => {
                // The simulators are unknown for now: keep the ones the tab
                // has, less one reported gone (a newer list brings it back
                // if it really runs).
                let gone = self.gone.as_ref().map(|(g, _)| g.clone());
                let mut list: Vec<DeviceInfo> = self
                    .booted
                    .iter()
                    .filter(|d| d.platform == Platform::Ios && Some(&d.udid) != gone.as_ref())
                    .cloned()
                    .collect();
                list.extend(android);
                if list == self.booted && !matches!(self.view, DeviceView::Android { .. }) {
                    // An unchanged Android list still counts for Android retries.
                    self.on_list(ticket, Err(ListError::Failed(e)))
                } else {
                    self.apply_list(ticket, Ok(list), false)
                }
            }
        }
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
        } else if matches!(self.view, DeviceView::Android { why: Some(_), .. }) {
            // Picking the failed emulator shown is asking to try again,
            // retries used up or not.
            if let Some(again) = self.retried() {
                self.view = again;
            }
        }
        self.reconcile()
    }

    /// The live view couldn't start (or ended): screenshots instead. For
    /// good when the display API is unsupported; otherwise for this start
    /// only (see [`Retry`]). An emulator's never falls back: its note shows
    /// instead, and it is retried the same way.
    pub(crate) fn on_stream_failed(&mut self, err: StreamError) -> Vec<Command> {
        let starting = matches!(err, StreamError::Starting(_));
        let (why, transient) = match err {
            StreamError::DeviceNotFound => return self.on_gone(),
            StreamError::NoXcode => ("Xcode not found".to_string(), true),
            StreamError::Unsupported(s) => (s.to_string(), false),
            StreamError::Failed(s) | StreamError::Starting(s) => (s, true),
        };
        if let DeviceView::Android { udid, why: None, .. } = &self.view {
            let udid = udid.clone();
            if starting {
                self.next_list(&udid);
            } else {
                self.failed_again(&udid);
            }
            self.view = DeviceView::Android { udid, why: Some(why), starting };
            return self.reconcile();
        }
        let DeviceView::Live { udid } = &self.view else {
            return Vec::new();
        };
        let udid = udid.clone();
        if transient {
            self.failed_again(&udid);
        } else {
            self.reduced_why = Some(why.clone());
        }
        self.view = DeviceView::Reduced { udid, why, transient };
        self.reconcile()
    }

    /// Another transient failure of `udid`'s stream: how many lists until
    /// it's tried again.
    fn failed_again(&mut self, udid: &str) {
        let failures = match &self.retry {
            Some(r) if r.udid == udid => r.failures + 1,
            _ => 1,
        };
        let lists_left = if failures <= LIST_RETRIES { 1 << (failures - 1) } else { 0 };
        self.retry = Some(Retry { udid: udid.to_string(), failures, lists_left });
    }

    /// `udid` is tried again on the next list, without counting as a
    /// failure (an emulator still booting).
    fn next_list(&mut self, udid: &str) {
        let failures = match &self.retry {
            Some(r) if r.udid == udid => r.failures,
            _ => 0,
        };
        self.retry = Some(Retry { udid: udid.to_string(), failures, lists_left: 1 });
    }

    /// The view to try live again from, for a transient fallback or an
    /// emulator whose stream failed; None for any other view.
    fn retried(&self) -> Option<DeviceView> {
        match &self.view {
            DeviceView::Reduced { udid, transient: true, .. } => {
                Some(DeviceView::Live { udid: udid.clone() })
            }
            DeviceView::Android { udid, why: Some(_), .. } => {
                Some(DeviceView::Android { udid: udid.clone(), why: None, starting: false })
            }
            _ => None,
        }
    }

    /// Another good list while a transient fallback (or a failed emulator
    /// stream) shows: live again once its wait is over.
    fn count_down_retry(&mut self) {
        let Some(again) = self.retried() else {
            return;
        };
        let (DeviceView::Live { udid } | DeviceView::Android { udid, .. }) = &again else {
            return;
        };
        let Some(retry) = self.retry.as_mut().filter(|r| &r.udid == udid && r.lists_left > 0)
        else {
            return;
        };
        retry.lists_left -= 1;
        if retry.lists_left == 0 {
            self.view = again;
        }
    }

    pub(crate) fn on_gone(&mut self) -> Vec<Command> {
        let (DeviceView::Live { udid }
        | DeviceView::Reduced { udid, .. }
        | DeviceView::Android { udid, .. }) = &self.view
        else {
            return Vec::new();
        };
        let name = self.name_of(udid);
        self.gone = Some((udid.clone(), self.lists_started));
        self.view = DeviceView::ShutDown { name };
        self.reconcile()
    }

    /// `udid`'s name as last listed, else the id.
    fn name_of(&self, udid: &str) -> String {
        self.booted
            .iter()
            .find(|d| d.udid == udid)
            .map_or_else(|| udid.to_string(), |d| d.name.clone())
    }

    /// The live view started (for an emulator: its first frame came): a
    /// later failure starts its backoff afresh.
    // Elsewhere than macOS no stream starts.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn on_stream_started(&mut self) {
        self.retry = None;
    }

    pub(crate) fn on_hidden(&mut self) -> Vec<Command> {
        self.hidden = true;
        self.reconcile()
    }

    /// The picture is covered (`true`) or shown again. Unlike
    /// [`Self::on_hidden`] this is not about the tab being on screen, so the
    /// caller keeps listing.
    pub(crate) fn on_obscured(&mut self, obscured: bool) -> Vec<Command> {
        self.obscured = obscured;
        self.reconcile()
    }

    /// Back on screen. A transient fallback (or a failed emulator stream)
    /// tries live again.
    pub(crate) fn on_shown(&mut self) -> Vec<Command> {
        self.hidden = false;
        if let Some(again) = self.retried() {
            self.view = again;
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
            ..Default::default()
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
    fn obscuring_stops_the_stream_and_uncovering_resumes_it() {
        let mut t = tab_on(&["a"]);
        assert_eq!(t.on_obscured(true), vec![Command::StopStream]);
        assert!(t.on_obscured(true).is_empty());
        assert_eq!(t.on_obscured(false), vec![Command::StartStream("a".into())]);
    }

    #[test]
    fn obscuring_stops_screenshots_too() {
        let mut t = tab_on(&["a"]);
        t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(t.on_obscured(true), vec![Command::StopScreenshots]);
        assert_eq!(t.on_obscured(false), vec![Command::StartScreenshots("a".into())]);
    }

    #[test]
    fn lists_update_state_while_obscured_but_start_nothing() {
        let mut t = tab_on(&["a"]);
        t.on_obscured(true);
        assert!(fresh(&mut t, Ok(vec![dev("b")])).is_empty());
        assert_eq!(t.view, live("b"));
        assert_eq!(t.selected.as_deref(), Some("b"));
        // A selection made while obscured starts nothing either.
        assert!(fresh(&mut t, Ok(vec![dev("a"), dev("b")])).is_empty());
        assert!(t.on_select("a").is_empty());
        // Hidden and obscured are separate: both must clear to run again.
        t.on_hidden();
        assert!(t.on_obscured(false).is_empty());
        assert_eq!(t.on_shown(), vec![Command::StartStream("a".into())]);
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

    fn android(udid: &str) -> DeviceInfo {
        DeviceInfo {
            udid: format!("android:{udid}"),
            name: format!("name-{udid}"),
            os: "Android".into(),
            platform: crate::devices::Platform::Android,
            ..Default::default()
        }
    }

    fn watching(udid: &str) -> DeviceView {
        DeviceView::Android { udid: format!("android:{udid}"), why: None, starting: false }
    }

    fn start(udid: &str) -> Command {
        Command::StartStream(udid.into())
    }

    #[test]
    fn a_replacement_run_of_the_same_avd_restarts_its_stream() {
        let mut t = DeviceTab::new();
        let run = |pid| {
            let mut dev = android("p");
            dev.android = Some(AndroidRunning {
                pid, avd_id: "p".into(), name: "name-p".into(), grpc_port: 8554,
                token: Some(format!("token-{pid}")), serial_port: 5554, avd_dir: "/p".into(),
            });
            dev
        };
        let first = run(10);
        fresh(&mut t, Ok(vec![first.clone()]));
        assert!(t.on_select("android:p").is_empty());
        assert!(fresh(&mut t, Ok(vec![first])).is_empty());
        assert_eq!(t.android_stream_target().map(|r| r.pid), Some(10));

        assert_eq!(fresh(&mut t, Ok(vec![run(11)])), vec![Command::StopStream, start("android:p")]);
        assert_eq!(t.preferred.as_deref(), Some("android:p"));
        assert_eq!(t.android_stream_target().map(|r| r.pid), Some(11));

        t.on_stream_failed(StreamError::Failed("x".into()));
        assert_eq!(fresh(&mut t, Ok(vec![run(12)])), vec![start("android:p")]);
        assert_eq!(t.android_stream_target().map(|r| r.pid), Some(12));
    }

    #[test]
    fn an_android_device_streams_but_is_never_reduced() {
        // Only an emulator: its stream starts (the panel sends an Android
        // id to the emulator's stream, never to simctl), and stops while
        // the tab is away.
        let mut t = DeviceTab::new();
        assert_eq!(fresh(&mut t, Ok(vec![android("p")])), vec![start("android:p")]);
        assert_eq!(t.view, watching("p"));
        assert_eq!(t.selected.as_deref(), Some("android:p"));
        assert_eq!(t.on_hidden(), vec![Command::StopStream]);
        assert_eq!(t.on_shown(), vec![start("android:p")]);
        assert_eq!(t.on_obscured(true), vec![Command::StopStream]);
        assert_eq!(t.on_obscured(false), vec![start("android:p")]);
        // From a live simulator: its stream stops first.
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![dev("a"), android("p")]));
        assert_eq!(t.view, live("a"));
        assert_eq!(t.on_select("android:p"), vec![Command::StopStream, start("android:p")]);
        assert_eq!(t.view, watching("p"));
        assert!(fresh(&mut t, Ok(vec![dev("a"), android("p")])).is_empty());
        // And back.
        assert_eq!(t.on_select("a"), vec![Command::StopStream, start("a")]);
        // An Xcode whose display API is unsupported is no reason to reduce
        // an emulator.
        t.on_stream_failed(StreamError::Unsupported("SimDeviceScreen"));
        assert_eq!(t.on_select("android:p"), vec![Command::StopScreenshots, start("android:p")]);
        assert_eq!(t.view, watching("p"));
        // Nor is a failing emulator stream ever a reason for screenshots.
        let cmds = t.on_stream_failed(StreamError::Unsupported("anything"));
        assert_eq!(cmds, vec![Command::StopStream]);
        assert!(matches!(&t.view, DeviceView::Android { why: Some(_), .. }), "{:?}", t.view);
        // Showing the tab tries it again; the emulator going away: shut
        // down, then the next device.
        t.on_hidden();
        assert_eq!(t.on_shown(), vec![start("android:p")]);
        assert_eq!(t.on_gone(), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-p".into() });
    }

    /// An emulator stream that ends while the emulator is still listed:
    /// its note shows, and it's tried again after 1, 2, 4 and 8 lists, then
    /// not until the tab shows again. Each restart is the panel's, from the
    /// newest list. Its first frame resets the backoff.
    #[test]
    fn an_emulator_stream_that_keeps_ending_is_retried_a_bounded_few_times() {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        let cmds = t.on_stream_failed(StreamError::Failed("Unavailable: closed".into()));
        assert_eq!(cmds, vec![Command::StopStream]);
        assert_eq!(
            t.view,
            DeviceView::Android {
                udid: "android:p".into(),
                why: Some("Unavailable: closed".into()),
                starting: false,
            }
        );
        let retry_after = |t: &mut DeviceTab| {
            (1..=100).find(|_| {
                let cmds = fresh(t, Ok(vec![android("p")]));
                assert!(cmds.iter().all(|c| *c == start("android:p")), "{cmds:?}");
                !cmds.is_empty()
            })
        };
        let mut waits = vec![retry_after(&mut t)];
        for _ in 0..5 {
            t.on_stream_failed(StreamError::Failed("x".into()));
            waits.push(retry_after(&mut t));
        }
        assert_eq!(waits, vec![Some(1), Some(2), Some(4), Some(8), None, None]);
        // Showing the tab again tries again, and a frame resets the wait.
        t.on_hidden();
        assert_eq!(t.on_shown(), vec![start("android:p")]);
        t.on_stream_started();
        t.on_stream_failed(StreamError::Failed("x".into()));
        assert_eq!(retry_after(&mut t), Some(1));
    }

    /// An emulator still booting (listed moments ago, not answering yet):
    /// "Starting…" rather than a failure, tried again on every list
    /// however many times, and none of it uses up the retries a real
    /// failure gets afterwards.
    #[test]
    fn a_booting_emulator_says_starting_and_keeps_its_retries() {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        for _ in 0..20 {
            let cmds = t.on_stream_failed(StreamError::Starting("not answering yet".into()));
            assert_eq!(cmds, vec![Command::StopStream]);
            assert_eq!(
                t.view,
                DeviceView::Android {
                    udid: "android:p".into(),
                    why: Some("not answering yet".into()),
                    starting: true,
                }
            );
            // One try per list: the next one.
            assert_eq!(fresh(&mut t, Ok(vec![android("p")])), vec![start("android:p")]);
            assert_eq!(t.view, watching("p"));
        }
        // A real failure after it starts its backoff from the first wait.
        t.on_stream_failed(StreamError::Failed("Unauthenticated: x".into()));
        assert!(matches!(t.view, DeviceView::Android { starting: false, why: Some(_), .. }));
        assert_eq!(fresh(&mut t, Ok(vec![android("p")])), vec![start("android:p")]);
        // Gone from the list while starting: shut down, and nothing more
        // for it.
        t.on_stream_failed(StreamError::Starting("not answering yet".into()));
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-p".into() });
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn an_unchanged_android_poll_advances_a_retry_when_simctl_times_out() {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        t.on_stream_failed(StreamError::Starting("not answering yet".into()));
        let ticket = t.list_started();
        assert_eq!(
            t.on_lists(ticket, Err(ListError::Failed("timeout".into())), vec![android("p")]),
            vec![start("android:p")],
        );
    }

    /// Picking the emulator already shown, once its stream failed, is an
    /// explicit retry, even after the list retries ran out. Picking it
    /// while it streams changes nothing.
    #[test]
    fn re_picking_a_failed_emulator_tries_it_again() {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        assert!(t.on_select("android:p").is_empty());
        // Fail each retry until the lists stop trying (after 4).
        let mut retries = 0;
        loop {
            t.on_stream_failed(StreamError::Failed("x".into()));
            if !(0..20).any(|_| !fresh(&mut t, Ok(vec![android("p")])).is_empty()) {
                break;
            }
            retries += 1;
        }
        assert_eq!(retries, 4);
        assert_eq!(t.on_select("android:p"), vec![start("android:p")]);
        assert_eq!(t.view, watching("p"));
    }

    /// An emulator that shut down mid-stream (its stream reports it gone):
    /// "shut down", as for a simulator; a list already under way that still
    /// has it doesn't restart it, and the list without it moves on. Nothing
    /// is ever started for it once it's gone.
    #[test]
    fn an_emulator_that_shut_down_mid_stream_is_not_restarted() {
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![dev("a"), android("p")]));
        t.on_select("android:p");
        let under_way = t.list_started();
        assert_eq!(t.on_gone(), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-p".into() });
        assert!(t.on_list(under_way, Ok(vec![dev("a"), android("p")])).is_empty());
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![start("a")]);
        // A failed stream whose emulator the next list drops: shut down,
        // then the list after moves on, and nothing starts for the
        // dropped one.
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p"), android("q")]));
        t.on_stream_failed(StreamError::Failed("Unavailable: closed".into()));
        assert!(fresh(&mut t, Ok(vec![android("q")])).is_empty());
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-p".into() });
        assert_eq!(fresh(&mut t, Ok(vec![android("q")])), vec![start("android:q")]);
        assert_eq!(t.view, watching("q"));
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        t.on_stream_failed(StreamError::Failed("Unavailable: closed".into()));
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    /// A watched emulator that drops out of the list has shut down, as a
    /// simulator's stream says when it goes ([`DeviceTab::on_gone`]):
    /// whether its stream was still waiting for the process to exit (one
    /// saving its Quick Boot snapshot takes seconds), had already ended
    /// without seeing it go, or the Mac has no Xcode. The next list moves
    /// on, as after any shutdown.
    #[test]
    fn a_watched_emulator_that_leaves_the_list_shut_down() {
        let shut_down = DeviceView::ShutDown { name: "name-p".into() };
        // Its stream still waiting: stopped, and "shut down".
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![dev("a"), android("p")]));
        t.on_select("android:p");
        let under_way = t.list_started();
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![Command::StopStream]);
        assert_eq!(t.view, shut_down);
        // A list fetched before it went doesn't bring it back.
        assert!(t.on_list(under_way, Ok(vec![dev("a"), android("p")])).is_empty());
        assert_eq!(t.view, shut_down);
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![start("a")]);
        // Its stream ended first ("Unavailable: Cancelling all calls")
        // while the emulator was still exiting.
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p"), android("q")]));
        t.on_stream_failed(StreamError::Failed("Unavailable: Cancelling all calls".into()));
        assert!(fresh(&mut t, Ok(vec![android("q")])).is_empty());
        assert_eq!(t.view, shut_down);
        assert_eq!(fresh(&mut t, Ok(vec![android("q")])), vec![start("android:q")]);
        // Still booting when it went.
        let mut t = DeviceTab::new();
        fresh(&mut t, Ok(vec![android("p")]));
        t.on_stream_failed(StreamError::Starting("not answering yet".into()));
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, shut_down);
        assert!(fresh(&mut t, Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
        // Android Studio without Xcode: "shut down" before "Needs Xcode".
        let mut t = DeviceTab::new();
        let ticket = t.list_started();
        t.on_lists(ticket, Err(ListError::NoXcode), vec![android("p")]);
        let ticket = t.list_started();
        assert_eq!(t.on_lists(ticket, Err(ListError::NoXcode), vec![]), vec![Command::StopStream]);
        assert_eq!(t.view, shut_down);
        let ticket = t.list_started();
        assert!(t.on_lists(ticket, Err(ListError::NoXcode), vec![]).is_empty());
        assert_eq!(t.view, DeviceView::NeedsXcode);
        // A simulator leaving the list is not that: its stream says when
        // it shut down.
        let mut t = tab_on(&["a"]);
        assert_eq!(fresh(&mut t, Ok(vec![])), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn emulators_show_without_xcode_or_a_working_simctl() {
        // No Xcode, an emulator running: it is watched.
        let mut t = DeviceTab::new();
        let ticket = t.list_started();
        assert_eq!(
            t.on_lists(ticket, Err(ListError::NoXcode), vec![android("p")]),
            vec![start("android:p")]
        );
        assert_eq!(t.view, watching("p"));
        // ...and once it stops, shut down, then the tab says Xcode is
        // needed.
        let ticket = t.list_started();
        assert_eq!(t.on_lists(ticket, Err(ListError::NoXcode), vec![]), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::ShutDown { name: "name-p".into() });
        let ticket = t.list_started();
        assert!(t.on_lists(ticket, Err(ListError::NoXcode), vec![]).is_empty());
        assert_eq!(t.view, DeviceView::NeedsXcode);
        // simctl works: simulators and emulators together, simulators first.
        let mut t = DeviceTab::new();
        let ticket = t.list_started();
        assert_eq!(
            t.on_lists(ticket, Ok(vec![dev("a")]), vec![android("p")]),
            vec![Command::StartStream("a".into())]
        );
        let ids = |t: &DeviceTab| t.booted.iter().map(|d| d.udid.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&t), ["a", "android:p"]);
        // simctl failing for now with nothing new: nothing changes.
        let failed = || Err(ListError::Failed("timeout".into()));
        let ticket = t.list_started();
        assert!(t.on_lists(ticket, failed(), vec![android("p")]).is_empty());
        assert_eq!(t.view, live("a"));
        // An emulator coming up meanwhile still shows; the simulators stay.
        let ticket = t.list_started();
        assert!(t.on_lists(ticket, failed(), vec![android("p"), android("q")]).is_empty());
        assert_eq!(ids(&t), ["a", "android:p", "android:q"]);
        assert_eq!(t.view, live("a"));
        // A simulator reported gone isn't brought back by a failed list.
        t.on_gone();
        let ticket = t.list_started();
        t.on_lists(ticket, failed(), vec![android("p")]);
        assert!(!ids(&t).contains(&"a".to_string()), "{:?}", ids(&t));
        assert_eq!(t.view, watching("p"));
    }

    #[test]
    fn an_android_list_change_does_not_advance_a_simulator_retry() {
        let mut t = tab_on(&["a"]);
        t.on_stream_failed(StreamError::Failed("simulator stream closed".into()));
        let ticket = t.list_started();
        assert!(t.on_lists(ticket, Err(ListError::Failed("timeout".into())), vec![android("p")]).is_empty());
        assert_eq!(t.view, reduced("a", "simulator stream closed", true));
        assert_eq!(fresh(&mut t, Ok(vec![dev("a")])), vec![
            Command::StopScreenshots,
            start("a"),
        ]);
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
