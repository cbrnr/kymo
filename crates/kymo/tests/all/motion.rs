//! Rows moving to their new places when channels are hidden, shown or scrolled.

use kymo::motion::{RowMotion, Slot};
use kymo::render::Scene;
use kymo::view::ViewState;
use kymo_core::ChannelId;

use crate::view::recording;

const SECONDS: f64 = 0.2;

/// Eight EEG channels, four rows on screen, followed at time 0.
fn start() -> (ViewState, RowMotion) {
    let mut view = ViewState::new(&recording(8, 60));
    view.set_rows(4);
    let mut motion = RowMotion::new();
    assert!(!motion.follow(&view, 0.0, SECONDS));
    (view, motion)
}

fn ids(rows: &[(ChannelId, Slot)]) -> Vec<u32> {
    rows.iter().map(|(c, _)| c.0).collect()
}

fn slot(rows: &[(ChannelId, Slot)], channel: u32) -> Slot {
    rows.iter().find(|(c, _)| c.0 == channel).unwrap().1
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The rows as the view has them, without motion.
fn settled(view: &ViewState) -> Vec<(u32, f64, f64)> {
    Scene::from_view(view, [100, 400], [1.0; 4])
        .rows
        .iter()
        .map(|r| (r.channel.0, r.center, r.height))
        .collect()
}

fn drawn(view: &ViewState, rows: &[(ChannelId, Slot)]) -> Vec<(u32, f64, f64)> {
    Scene::from_slots(view, rows, [100, 400], [1.0; 4])
        .rows
        .iter()
        .map(|r| (r.channel.0, r.center, r.height))
        .collect()
}

#[test]
fn a_hidden_row_collapses_while_the_next_ones_move_up() {
    let (mut view, mut motion) = start();
    let before = motion.rows(&view, 0.0);
    assert_eq!(ids(&before), [0, 1, 2, 3]);
    view.set_shown(ChannelId(1), false);

    // At first, everything is where it was.
    assert!(motion.follow(&view, 1.0, SECONDS));
    let first = motion.rows(&view, 1.0);
    assert_eq!(ids(&first[..4]), [0, 1, 2, 3]);
    for (c, s) in &before {
        assert_eq!(slot(&first, c.0), *s);
    }

    // Halfway, the hidden row is thinner and fainter, and the rows below are on their way
    // up, the row coming in from below the screen too.
    assert!(motion.follow(&view, 1.1, SECONDS));
    let half = motion.rows(&view, 1.1);
    assert_eq!(ids(&half), [0, 1, 2, 3, 4]);
    let gone = slot(&half, 1);
    assert!(gone.height > 0.0 && gone.height < 0.25);
    assert!(gone.opacity > 0.0 && gone.opacity < 1.0);
    assert!(close(gone.top, 0.25));
    let next = slot(&half, 2);
    assert!(next.top > 0.25 && next.top < 0.5);
    assert!(close(next.top, gone.top + gone.height));
    assert!(close(slot(&half, 0).top, 0.0));
    assert_eq!(slot(&half, 0).opacity, 1.0);

    // Then the rows are the view's.
    assert!(!motion.follow(&view, 1.25, SECONDS));
    let last = motion.rows(&view, 1.25);
    assert_eq!(ids(&last), [0, 2, 3, 4]);
    assert_eq!(drawn(&view, &last), settled(&view));
}

#[test]
fn rows_grow_into_the_room_when_fewer_are_left_than_fit() {
    let (mut view, mut motion) = start();
    for c in 3..10 {
        view.set_shown(ChannelId(c), false);
    }
    motion.follow(&view, 0.0, SECONDS);
    motion.follow(&view, 0.1, SECONDS);
    let half = motion.rows(&view, 0.1);
    let height = slot(&half, 0).height;
    assert!(height > 0.25 && height < 1.0 / 3.0);
    assert!(!motion.follow(&view, 0.3, SECONDS));
    assert_eq!(drawn(&view, &motion.rows(&view, 0.3)), settled(&view));
}

#[test]
fn a_row_shown_again_opens_from_nothing() {
    let (mut view, mut motion) = start();
    view.set_shown(ChannelId(2), false);
    motion.follow(&view, 0.0, SECONDS);
    motion.follow(&view, 1.0, SECONDS);
    view.set_shown(ChannelId(2), true);
    motion.follow(&view, 2.0, SECONDS);
    // Not drawn at first: it has no height yet.
    assert_eq!(ids(&motion.rows(&view, 2.0)), [0, 1, 3, 4]);
    let half = motion.rows(&view, 2.1);
    let back = slot(&half, 2);
    assert!(back.height > 0.0 && back.height < 0.25 && back.opacity < 1.0);
    motion.follow(&view, 2.3, SECONDS);
    assert_eq!(drawn(&view, &motion.rows(&view, 2.3)), settled(&view));
}

#[test]
fn a_change_midway_sets_off_from_where_the_rows_are() {
    let (mut view, mut motion) = start();
    view.set_shown(ChannelId(1), false);
    motion.follow(&view, 0.0, SECONDS);
    let midway = motion.rows(&view, 0.05);
    view.set_shown(ChannelId(1), true);
    motion.follow(&view, 0.05, SECONDS);
    let after = motion.rows(&view, 0.05);
    assert_eq!(ids(&after), ids(&midway));
    for ((_, a), (_, b)) in midway.iter().zip(&after) {
        assert!(close(a.top, b.top) && close(a.height, b.height));
        assert!(close(a.opacity, b.opacity));
    }
    motion.follow(&view, 1.0, SECONDS);
    assert_eq!(drawn(&view, &motion.rows(&view, 1.0)), settled(&view));
}

#[test]
fn scrolling_and_no_time_move_rows_at_once() {
    let (mut view, mut motion) = start();
    view.set_top(2.0);
    assert!(!motion.follow(&view, 0.1, SECONDS));
    assert_eq!(drawn(&view, &motion.rows(&view, 0.1)), settled(&view));

    view.set_shown(ChannelId(3), false);
    assert!(!motion.follow(&view, 0.2, 0.0));
    assert_eq!(drawn(&view, &motion.rows(&view, 0.2)), settled(&view));
}
