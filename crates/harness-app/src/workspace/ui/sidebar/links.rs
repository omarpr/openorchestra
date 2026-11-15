//! Live A2A connector lines between a task's coordinator tile and its repo
//! rows: which link is busy, and the short "message travelling" animation
//! when a delegation goes out (down) or its result comes back (up).
//!
//! Pure state and timing, so the rules are testable without a window. The
//! sidebar turns a [`LinkVisual`] into colour, opacity and a dot position.

use std::collections::HashMap;

/// One A2A row as the broker reports it: `(a2a_task_id, tenant, state)`.
pub(crate) type A2aRow = (String, String, String);

/// How long a message dot takes to travel the connector.
pub(crate) const TRAVEL_SECS: f32 = 1.0;
/// When the flash after a returning (`Up`) message has faded out.
pub(crate) const FLASH_END_SECS: f32 = 2.5;
/// One breath of an active link's pulse.
pub(crate) const ACTIVE_PULSE_PERIOD: f32 = 1.6;

/// Member row height in the task block, fixed so the connector geometry
/// below can be computed without layout feedback.
pub(crate) const ROW_H: f32 = 26.0;
/// The spine's x inside the task block's member gutter.
pub(crate) const SPINE_X: f32 = 12.0;
/// Where an elbow ends, just short of the row's text.
pub(crate) const ELBOW_END_X: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LinkEventKind {
    /// The coordinator delegated to this repo (tile → row).
    Down,
    /// The repo's delegation finished (row → tile); `ok` false for
    /// failed/rejected.
    Up { ok: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkEvent {
    pub task_id: String,
    pub tenant: String,
    pub kind: LinkEventKind,
    pub started: f32,
}

impl LinkEvent {
    /// How long after `started` this event still draws something.
    fn lifetime(&self) -> f32 {
        match self.kind {
            LinkEventKind::Down => TRAVEL_SECS,
            LinkEventKind::Up { .. } => FLASH_END_SECS,
        }
    }

    fn live_at(&self, now: f32) -> bool {
        let age = now - self.started;
        age >= 0.0 && age < self.lifetime()
    }
}

/// What one connector looks like right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LinkVisual {
    Idle,
    /// A delegation to this repo is in flight; `pulse` is 0..=1.
    Active { pulse: f32 },
    /// A message dot `progress` (0..1) of the way along the connector.
    Travel { kind: LinkEventKind, progress: f32 },
    /// Afterglow of a returned result; `fade` runs 1 → 0.
    Flash { ok: bool, fade: f32 },
}

fn is_active_state(state: &str) -> bool {
    state == "working" || state == "submitted"
}

/// The `Up` a terminal state means, if it is one.
fn terminal_kind(state: &str) -> Option<LinkEventKind> {
    match state {
        "completed" => Some(LinkEventKind::Up { ok: true }),
        "failed" | "rejected" => Some(LinkEventKind::Up { ok: false }),
        _ => None,
    }
}

/// Events implied by going from `prev` to `next` for one task: a new id is a
/// delegation sent (`Down`); an id that just became `completed` is `Up ok`,
/// `failed`/`rejected` is `Up` not ok. Anything else emits nothing. An id
/// first seen already finished (sent and done between two polls) gets both:
/// the `Down` now and its `Up` once that has travelled.
pub(crate) fn diff_snapshots(task_id: &str, prev: &[A2aRow], next: &[A2aRow], now: f32) -> Vec<LinkEvent> {
    let mut out = Vec::new();
    for (id, tenant, state) in next {
        let mut push = |kind, started| {
            out.push(LinkEvent {
                task_id: task_id.to_string(),
                tenant: tenant.clone(),
                kind,
                started,
            })
        };
        match prev.iter().find(|(pid, _, _)| pid == id) {
            None => {
                push(LinkEventKind::Down, now);
                if let Some(up) = terminal_kind(state) {
                    push(up, now + TRAVEL_SECS);
                }
            }
            Some((_, _, before)) if before == state => {}
            Some(_) => {
                if let Some(up) = terminal_kind(state) {
                    push(up, now);
                }
            }
        }
    }
    out
}

/// The connector's look at `now`: the newest live event wins (travel for the
/// first second, then the flash after an `Up`); otherwise a pulse while a
/// delegation is in flight, else idle.
pub(crate) fn link_visual(active: bool, events: &[&LinkEvent], now: f32) -> LinkVisual {
    let newest = events
        .iter()
        .filter(|e| e.live_at(now))
        .max_by(|a, b| a.started.total_cmp(&b.started));
    if let Some(e) = newest {
        let age = now - e.started;
        if age < TRAVEL_SECS {
            return LinkVisual::Travel {
                kind: e.kind,
                progress: age / TRAVEL_SECS,
            };
        }
        if let LinkEventKind::Up { ok } = e.kind {
            return LinkVisual::Flash {
                ok,
                fade: 1.0 - (age - TRAVEL_SECS) / (FLASH_END_SECS - TRAVEL_SECS),
            };
        }
    }
    if active {
        LinkVisual::Active {
            pulse: 0.5 + 0.5 * (now * std::f32::consts::TAU / ACTIVE_PULSE_PERIOD).sin(),
        }
    } else {
        LinkVisual::Idle
    }
}

/// Where a travelling dot's centre is, in the member gutter's coordinates
/// (origin at the top-left of the members block): down the spine to row
/// `row`'s middle, then out along its elbow. `Up` runs the same path
/// backwards.
pub(crate) fn dot_position(row: usize, progress: f32, kind: LinkEventKind) -> (f32, f32) {
    let mid_y = row as f32 * ROW_H + ROW_H / 2.0;
    let elbow = ELBOW_END_X - SPINE_X;
    let total = mid_y + elbow;
    let p = progress.clamp(0.0, 1.0);
    let d = match kind {
        LinkEventKind::Down => p,
        LinkEventKind::Up { .. } => 1.0 - p,
    } * total;
    if d <= mid_y {
        (SPINE_X, d)
    } else {
        (SPINE_X + (d - mid_y), mid_y)
    }
}

/// Remembers each task's last A2A snapshot and the recent events between
/// snapshots, so the sidebar can animate them.
#[derive(Debug, Default)]
pub(crate) struct LinkTracker {
    prev: HashMap<String, Vec<A2aRow>>,
    events: Vec<LinkEvent>,
}

impl LinkTracker {
    /// Record `rows` as `task_id`'s current snapshot, turning the difference
    /// from the last one into events. A task seen for the first time diffs
    /// against nothing: brokers start empty, so its rows are all new.
    pub(crate) fn observe(&mut self, task_id: &str, rows: Vec<A2aRow>, now: f32) {
        self.prune(now);
        let prev = self.prev.get(task_id).map(Vec::as_slice).unwrap_or(&[]);
        let new_events = diff_snapshots(task_id, prev, &rows, now);
        self.events.extend(new_events);
        self.prev.insert(task_id.to_string(), rows);
    }

    /// Forget tasks whose broker is gone, so a delegation left "working"
    /// when its task ended doesn't keep the animation running.
    pub(crate) fn retain_tasks(&mut self, keep: impl Fn(&str) -> bool) {
        self.prev.retain(|tid, _| keep(tid));
        self.events.retain(|e| keep(&e.task_id));
    }

    pub(crate) fn events_for(&self, task_id: &str, tenant: &str) -> Vec<&LinkEvent> {
        self.events
            .iter()
            .filter(|e| e.task_id == task_id && e.tenant == tenant)
            .collect()
    }

    /// Whether any event still draws something at `now`.
    pub(crate) fn is_animating(&self, now: f32) -> bool {
        self.events.iter().any(|e| e.live_at(now))
    }

    /// Whether any recorded delegation is in flight (its link pulses).
    pub(crate) fn any_active(&self) -> bool {
        self.prev
            .values()
            .flatten()
            .any(|(_, _, s)| is_active_state(s))
    }

    /// Drop events past their last frame.
    pub(crate) fn prune(&mut self, now: f32) {
        self.events.retain(|e| now - e.started < FLASH_END_SECS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, tenant: &str, state: &str) -> A2aRow {
        (id.into(), tenant.into(), state.into())
    }

    fn up_ok_at(started: f32) -> LinkEvent {
        LinkEvent {
            task_id: "t".into(),
            tenant: "wt-a".into(),
            kind: LinkEventKind::Up { ok: true },
            started,
        }
    }

    #[test]
    fn new_delegation_is_a_down_event() {
        let ev = diff_snapshots("t", &[], &[row("a1", "wt-a", "working")], 3.0);
        assert_eq!(
            ev,
            vec![LinkEvent {
                task_id: "t".into(),
                tenant: "wt-a".into(),
                kind: LinkEventKind::Down,
                started: 3.0,
            }]
        );
    }

    #[test]
    fn completion_is_an_up_ok_event() {
        let ev = diff_snapshots(
            "t",
            &[row("a1", "wt-a", "working")],
            &[row("a1", "wt-a", "completed")],
            1.0,
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, LinkEventKind::Up { ok: true });
        assert_eq!(ev[0].tenant, "wt-a");
    }

    #[test]
    fn failure_is_an_up_not_ok_event() {
        for end in ["failed", "rejected"] {
            let ev = diff_snapshots(
                "t",
                &[row("a1", "wt-a", "working")],
                &[row("a1", "wt-a", end)],
                1.0,
            );
            assert_eq!(ev.len(), 1, "{end}");
            assert_eq!(ev[0].kind, LinkEventKind::Up { ok: false }, "{end}");
        }
    }

    #[test]
    fn new_and_already_completed_emits_down_then_up() {
        let ev = diff_snapshots("t", &[], &[row("a1", "wt-a", "completed")], 4.0);
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev[0].kind, LinkEventKind::Down);
        assert_eq!(ev[0].started, 4.0);
        assert_eq!(ev[1].kind, LinkEventKind::Up { ok: true });
        assert_eq!(ev[1].started, 4.0 + TRAVEL_SECS);
        let refs: Vec<&LinkEvent> = ev.iter().collect();
        assert!(matches!(
            link_visual(false, &refs, 4.5),
            LinkVisual::Travel { kind: LinkEventKind::Down, .. }
        ));
        assert!(matches!(
            link_visual(false, &refs, 5.5),
            LinkVisual::Travel { kind: LinkEventKind::Up { ok: true }, .. }
        ));
        // Failed/rejected the same way, not ok.
        let ev = diff_snapshots("t", &[], &[row("a2", "wt-b", "rejected")], 0.0);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1].kind, LinkEventKind::Up { ok: false });
    }

    #[test]
    fn unchanged_rows_emit_nothing() {
        let rows = vec![row("a1", "wt-a", "working"), row("a2", "wt-b", "completed")];
        assert!(diff_snapshots("t", &rows, &rows, 1.0).is_empty());
        // submitted → working is progress, not a message on the wire.
        assert!(
            diff_snapshots("t", &[row("a1", "wt-a", "submitted")], &[row("a1", "wt-a", "working")], 1.0)
                .is_empty()
        );
    }

    #[test]
    fn travel_then_flash_then_idle() {
        let e = up_ok_at(0.0);
        match link_visual(false, &[&e], 0.5) {
            LinkVisual::Travel { kind, progress } => {
                assert_eq!(kind, LinkEventKind::Up { ok: true });
                assert!((progress - 0.5).abs() < 1e-4);
            }
            other => panic!("expected Travel, got {other:?}"),
        }
        match link_visual(false, &[&e], 1.5) {
            LinkVisual::Flash { ok, fade } => {
                assert!(ok);
                assert!(fade > 0.0 && fade < 1.0, "fade {fade}");
            }
            other => panic!("expected Flash, got {other:?}"),
        }
        assert_eq!(link_visual(false, &[&e], 3.0), LinkVisual::Idle);
    }

    #[test]
    fn newest_live_event_wins() {
        let old = up_ok_at(0.0);
        let down = LinkEvent {
            kind: LinkEventKind::Down,
            started: 1.2,
            ..up_ok_at(0.0)
        };
        assert!(matches!(
            link_visual(false, &[&old, &down], 1.5),
            LinkVisual::Travel { kind: LinkEventKind::Down, .. }
        ));
    }

    #[test]
    fn active_without_events_pulses() {
        match link_visual(true, &[], 0.4) {
            LinkVisual::Active { pulse } => {
                // 0.4s is a quarter of the 1.6s period: the sine's peak.
                assert!((pulse - 1.0).abs() < 1e-4, "pulse {pulse}");
            }
            other => panic!("expected Active, got {other:?}"),
        }
        assert_eq!(link_visual(false, &[], 0.4), LinkVisual::Idle);
    }

    #[test]
    fn tracker_stops_animating_after_events_expire() {
        let mut tr = LinkTracker::default();
        tr.observe("t", vec![row("a1", "wt-a", "working")], 0.0);
        tr.observe("t", vec![row("a1", "wt-a", "completed")], 0.1);
        assert!(tr.is_animating(0.5));
        assert_eq!(tr.events_for("t", "wt-a").len(), 2);
        assert!(tr.events_for("t", "wt-b").is_empty());
        assert!(!tr.any_active());
        assert!(!tr.is_animating(2.6));
        tr.prune(2.7);
        assert!(tr.events_for("t", "wt-a").is_empty());
    }

    #[test]
    fn tracker_forgets_ended_tasks() {
        let mut tr = LinkTracker::default();
        tr.observe("t", vec![row("a1", "wt-a", "working")], 0.0);
        assert!(tr.any_active());
        tr.retain_tasks(|tid| tid != "t");
        assert!(!tr.any_active());
        assert!(!tr.is_animating(0.1));
    }

    #[test]
    fn dot_runs_down_the_spine_then_out_the_elbow() {
        // Row 1's middle is 1.5 rows down; the elbow adds its width.
        let mid = 1.5 * ROW_H;
        let total = mid + (ELBOW_END_X - SPINE_X);
        assert_eq!(dot_position(1, 0.0, LinkEventKind::Down), (SPINE_X, 0.0));
        let (x, y) = dot_position(1, (mid / 2.0) / total, LinkEventKind::Down);
        assert!((x - SPINE_X).abs() < 1e-3 && (y - mid / 2.0).abs() < 1e-3);
        let (x, y) = dot_position(1, 1.0, LinkEventKind::Down);
        assert!((x - ELBOW_END_X).abs() < 1e-3 && (y - mid).abs() < 1e-3);
        // Up starts at the row and ends at the tile.
        assert_eq!(dot_position(1, 1.0, LinkEventKind::Up { ok: true }), (SPINE_X, 0.0));
    }
}
