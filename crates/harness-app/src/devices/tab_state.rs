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
    Reduced { udid: String, why: String },
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

pub(crate) struct DeviceTab {
    pub booted: Vec<DeviceInfo>,
    pub selected: Option<String>,
    pub view: DeviceView,
    hidden: bool,
    running: Running,
    /// Set once the private stream failed on this Xcode; later devices go
    /// straight to screenshots because the failure is about Xcode, not a device.
    reduced_why: Option<String>,
    /// A device reported gone; stays shut down while a stale list still has it.
    gone_udid: Option<String>,
}

impl DeviceTab {
    pub(crate) fn new() -> Self {
        Self {
            booted: Vec::new(),
            selected: None,
            view: DeviceView::NoneRunning,
            hidden: false,
            running: Running::Nothing,
            reduced_why: None,
            gone_udid: None,
        }
    }

    fn view_for(&self, udid: &str) -> DeviceView {
        match &self.reduced_why {
            Some(why) => DeviceView::Reduced { udid: udid.into(), why: why.clone() },
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

    pub(crate) fn on_list(&mut self, list: Result<Vec<DeviceInfo>, ListError>) -> Vec<Command> {
        match list {
            Err(ListError::NoXcode) => {
                self.booted.clear();
                self.selected = None;
                self.gone_udid = None;
                self.view = DeviceView::NeedsXcode;
            }
            // A transient hiccup must not tear down a working stream.
            Err(ListError::Failed(_)) => return Vec::new(),
            Ok(list) => {
                self.booted = list;
                if let Some(g) = &self.gone_udid
                    && !self.booted.iter().any(|d| &d.udid == g)
                {
                    self.gone_udid = None;
                }
                let chosen = choose_device(&self.booted, self.selected.as_deref())
                    .map(|d| d.udid.clone());
                self.selected = chosen.clone();
                match chosen {
                    None => self.view = DeviceView::NoneRunning,
                    Some(udid) if self.gone_udid.as_deref() == Some(udid.as_str()) => {}
                    Some(udid) => {
                        if !self.shows(&udid) {
                            self.view = self.view_for(&udid);
                        }
                    }
                }
            }
        }
        self.reconcile()
    }

    pub(crate) fn on_select(&mut self, udid: &str) -> Vec<Command> {
        if !self.booted.iter().any(|d| d.udid == udid) {
            return Vec::new();
        }
        self.selected = Some(udid.into());
        self.gone_udid = None;
        if !self.shows(udid) {
            self.view = self.view_for(udid);
        }
        self.reconcile()
    }

    pub(crate) fn on_stream_failed(&mut self, err: StreamError) -> Vec<Command> {
        let why = match err {
            StreamError::DeviceNotFound => return self.on_gone(),
            StreamError::NoXcode => "Xcode not found".to_string(),
            StreamError::Unsupported(s) => s.to_string(),
            StreamError::Failed(s) => s,
        };
        let DeviceView::Live { udid } = &self.view else {
            return Vec::new();
        };
        self.view = DeviceView::Reduced { udid: udid.clone(), why: why.clone() };
        self.reduced_why = Some(why);
        self.reconcile()
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
        self.gone_udid = Some(udid.clone());
        self.view = DeviceView::ShutDown { name };
        self.reconcile()
    }

    pub(crate) fn on_hidden(&mut self) -> Vec<Command> {
        self.hidden = true;
        self.reconcile()
    }

    pub(crate) fn on_shown(&mut self) -> Vec<Command> {
        self.hidden = false;
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

    fn tab_on(udids: &[&str]) -> DeviceTab {
        let mut t = DeviceTab::new();
        t.on_list(Ok(udids.iter().map(|u| dev(u)).collect()));
        t
    }

    #[test]
    fn first_list_starts_a_stream_on_the_first_device() {
        let mut t = DeviceTab::new();
        assert_eq!(t.on_list(Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
        assert_eq!(t.view, live("a"));
        assert_eq!(t.selected.as_deref(), Some("a"));
    }

    #[test]
    fn no_xcode_shows_needs_xcode_and_starts_nothing() {
        let mut t = DeviceTab::new();
        assert!(t.on_list(Err(ListError::NoXcode)).is_empty());
        assert_eq!(t.view, DeviceView::NeedsXcode);
    }

    #[test]
    fn no_xcode_stops_what_runs() {
        let mut t = tab_on(&["a"]);
        assert_eq!(t.on_list(Err(ListError::NoXcode)), vec![Command::StopStream]);
        assert_eq!(t.view, DeviceView::NeedsXcode);
    }

    #[test]
    fn an_empty_list_shows_none_running() {
        let mut t = DeviceTab::new();
        assert!(t.on_list(Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn a_failed_list_keeps_the_current_view() {
        let mut t = tab_on(&["a"]);
        assert!(t.on_list(Err(ListError::Failed("timeout".into()))).is_empty());
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
            DeviceView::Reduced { udid: "a".into(), why: "SimDeviceScreen".into() }
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
        assert!(t.on_list(Ok(vec![])).is_empty());
        assert_eq!(t.view, DeviceView::NoneRunning);
    }

    #[test]
    fn a_stale_list_does_not_revive_a_gone_device() {
        let mut t = tab_on(&["a"]);
        t.on_gone();
        assert!(t.on_list(Ok(vec![dev("a")])).is_empty());
        assert!(matches!(t.view, DeviceView::ShutDown { .. }));
        t.on_list(Ok(vec![]));
        assert_eq!(t.on_list(Ok(vec![dev("a")])), vec![Command::StartStream("a".into())]);
    }

    #[test]
    fn a_vanished_selection_moves_to_the_next_booted_device() {
        let mut t = tab_on(&["a"]);
        assert_eq!(
            t.on_list(Ok(vec![dev("b")])),
            vec![Command::StopStream, Command::StartStream("b".into())]
        );
        assert_eq!(t.view, live("b"));
        assert_eq!(t.selected.as_deref(), Some("b"));
    }

    #[test]
    fn the_same_list_again_issues_no_commands() {
        let mut t = tab_on(&["a"]);
        assert!(t.on_list(Ok(vec![dev("a")])).is_empty());
        t.on_hidden();
        assert!(t.on_list(Ok(vec![dev("a")])).is_empty());
        assert!(t.on_list(Ok(vec![dev("a")])).is_empty());
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
        assert!(t.on_list(Ok(vec![dev("b")])).is_empty());
        assert_eq!(t.view, live("b"));
        assert_eq!(t.on_shown(), vec![Command::StartStream("b".into())]);
    }

    #[test]
    fn showing_resumes_screenshots_when_reduced() {
        let mut t = tab_on(&["a"]);
        t.on_stream_failed(StreamError::Failed("x".into()));
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
}
