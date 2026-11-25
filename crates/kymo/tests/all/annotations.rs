//! The annotation legend and the overlay's marks, checked against brute force over the
//! annotations.

use std::collections::{BTreeSet, HashMap};

use kymo::annotations::AnnotationColor::{self, Palette};
use kymo::annotations::{
    COLORS, Legend, Mark, Marks, Span, marks, overview_marks, overview_under, span_y, under,
};
use kymo::view::ViewState;
use kymo_core::testing::{MemData, single_grid};
use kymo_core::{
    Annotation, AnnotationData, AnnotationOp, AnnotationStore, ChannelId, Origin, Recording,
};
use proptest::prelude::*;

/// 8 channels at 100 Hz over 600 s.
fn recording() -> Recording {
    let data = MemData::I16(vec![vec![0; 60_000]; 8]);
    Recording::new(single_grid(100.0, data)).unwrap().0
}

fn data(onset: f64, duration: f64, description: &str, channels: &[u32]) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: description.into(),
        channels: channels.iter().copied().map(ChannelId).collect(),
        origin: Origin::File,
        stream: None,
    }
}

fn insert(store: &mut AnnotationStore, data: AnnotationData) {
    let id = store.new_id();
    store
        .apply(AnnotationOp::Insert(Annotation { id, data }))
        .unwrap();
}

fn colors(legend: &Legend) -> Vec<(String, usize, usize)> {
    legend
        .entries()
        .map(|e| (e.description.clone(), e.slot, e.count))
        .collect()
}

#[test]
fn colors_follow_first_appearance_and_never_change() {
    let mut store = AnnotationStore::from_data(vec![
        data(5.0, 0.0, "b", &[]),
        data(1.0, 2.0, "a", &[]),
        data(9.0, 0.0, "b", &[]),
    ]);
    let mut legend = Legend::new(&store, false);
    let own = |d: &str, c: usize, n: usize| (d.to_owned(), c, n);
    assert_eq!(colors(&legend), [own("a", 0, 1), own("b", 1, 2)]);

    // New descriptions take the next colors, even when they come earlier in time; the
    // known ones keep theirs, and their shown state.
    legend.set_shown(None, "b", false);
    insert(&mut store, data(0.5, 0.0, "c", &[]));
    insert(&mut store, data(0.2, 0.0, "d", &[]));
    insert(&mut store, data(3.0, 0.0, "a", &[]));
    legend.sync(&store);
    assert_eq!(
        colors(&legend),
        [
            own("a", 0, 2),
            own("b", 1, 2),
            own("d", 2, 1),
            own("c", 3, 1)
        ]
    );
    assert!(!legend.entry(None, "b").unwrap().shown);
    assert!(legend.entry(None, "d").unwrap().shown);

    // A description whose annotations are all gone leaves the legend and keeps its color.
    let ids: Vec<_> = store
        .iter()
        .filter(|a| a.data.description == "a")
        .map(|a| a.id)
        .collect();
    for id in ids {
        store.apply(AnnotationOp::Remove(id)).unwrap();
    }
    legend.sync(&store);
    assert_eq!(
        colors(&legend),
        [own("b", 1, 2), own("d", 2, 1), own("c", 3, 1)]
    );
    insert(&mut store, data(7.0, 0.0, "a", &[]));
    legend.sync(&store);
    assert_eq!(legend.entry(None, "a").unwrap().slot, 0);

    // Colors cycle: a, b, d and c take 0 to 3, x0 to x5 take 4 to 9, x6 takes 0 again.
    for k in 0..COLORS {
        insert(
            &mut store,
            data(20.0 + k as f64, 0.0, &format!("x{k}"), &[]),
        );
    }
    legend.sync(&store);
    assert_eq!(legend.entry(None, "x6").unwrap().slot, 0);
}

#[test]
fn descriptions_show_and_hide() {
    let store = AnnotationStore::from_data(vec![
        data(1.0, 0.0, "a", &[]),
        data(2.0, 0.0, "b", &[]),
        data(3.0, 0.0, "c", &[]),
    ]);
    let mut legend = Legend::new(&store, false);
    assert_eq!(legend.shown_count(), (3, 3));
    legend.set_shown(None, "b", false);
    assert_eq!(legend.shown_count(), (2, 3));
    legend.set_all_shown(false);
    assert_eq!(legend.shown_count(), (0, 3));
    legend.set_all_shown(true);
    assert_eq!(legend.shown_count(), (3, 3));
    assert!(!legend.is_empty());
    assert!(Legend::new(&AnnotationStore::default(), false).is_empty());
}

/// Columns of each color and span, from bands and lines separately.
type Coverage = HashMap<(AnnotationColor, Span), (BTreeSet<u32>, BTreeSet<u32>)>;

/// What `marks` must cover, from every annotation in turn.
fn brute_force(view: &ViewState, store: &AnnotationStore, legend: &Legend, width: u32) -> Coverage {
    let mut cover = Coverage::new();
    let t = view.time();
    let w = f64::from(width);
    for a in store.iter() {
        let d = &a.data;
        let entry = legend.of(d).unwrap();
        if !entry.shown || d.onset >= t.end || d.end() < t.start {
            continue;
        }
        // Stim annotations mark a moment for all channels.
        let spans: Vec<Span> = if d.channels.is_empty() || d.origin == Origin::Stim {
            vec![Span::All]
        } else {
            d.channels
                .iter()
                .filter_map(|&c| view.row_of(c))
                .filter(|r| view.visible_rows().contains(r))
                .map(Span::Row)
                .collect()
        };
        let x0 = view.x_at(d.onset, w);
        let x1 = view.x_at(d.end(), w);
        let first = x0.floor().clamp(0.0, w - 1.0) as u32;
        for span in spans {
            let (bands, lines) = cover.entry((entry.color, span)).or_default();
            if x1 - x0 < 1.0 {
                // Shorter than a pixel: the column of its start.
                lines.insert(first);
            } else {
                // Every column it touches.
                let end = (x1.ceil().min(w) as u32).max(first + 1);
                bands.extend(first..end);
            }
        }
    }
    cover
}

fn check(marks: &Marks, cover: &Coverage, width: u32) -> Result<(), TestCaseError> {
    let mut got = Coverage::new();
    let mut by_key: HashMap<(AnnotationColor, Span), Vec<&Mark>> = HashMap::new();
    for band in &marks.bands {
        prop_assert!(
            !band.columns.is_empty() && band.columns.end <= width,
            "{band:?}"
        );
        by_key
            .entry((band.color, band.span))
            .or_default()
            .push(band);
        got.entry((band.color, band.span))
            .or_default()
            .0
            .extend(band.columns.clone());
    }
    // Bands of a color and span neither overlap nor touch.
    for bands in by_key.values_mut() {
        bands.sort_by_key(|b| b.columns.start);
        for pair in bands.windows(2) {
            prop_assert!(pair[0].columns.end < pair[1].columns.start, "{pair:?}");
        }
    }
    for line in &marks.lines {
        prop_assert_eq!(line.columns.len(), 1);
        prop_assert!(line.columns.end <= width);
        let lines = &mut got.entry((line.color, line.span)).or_default().1;
        prop_assert!(lines.insert(line.columns.start), "two lines at {line:?}");
    }
    got.retain(|_, (b, l)| !b.is_empty() || !l.is_empty());
    let mut want = cover.clone();
    want.retain(|_, (b, l)| !b.is_empty() || !l.is_empty());
    prop_assert_eq!(got, want);
    Ok(())
}

fn label_width(text: &str) -> f64 {
    7.0 * text.chars().count() as f64 + 5.0
}

/// Labels don't overlap, come from shown annotations in view, and one is skipped only where
/// the label before it takes the room.
fn check_labels(
    marks: &Marks,
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width: u32,
) -> Result<(), TestCaseError> {
    let w = f64::from(width);
    let t = view.time();
    // Shown annotations in view on a visible row, by onset.
    let candidates: Vec<(f64, &str, AnnotationColor)> = store
        .iter()
        .filter(|a| a.data.onset < t.end && a.data.end() >= t.start)
        .filter(|a| legend.of(&a.data).unwrap().shown)
        .filter(|a| {
            a.data.channels.is_empty()
                || a.data.origin == Origin::Stim
                || a.data.channels.iter().any(|&c| {
                    view.row_of(c)
                        .is_some_and(|r| view.visible_rows().contains(&r))
                })
        })
        .map(|a| {
            let x = view.x_at(a.data.onset, w).max(0.0);
            let color = legend.of(&a.data).unwrap().color;
            (x, a.data.description.as_str(), color)
        })
        .collect();
    let mut labels = marks.labels.iter().peekable();
    let mut room_until = f64::NEG_INFINITY;
    for (x, text, color) in candidates {
        match labels.peek() {
            Some(l) if l.x == x && l.text == text => {
                prop_assert!(x >= room_until, "{l:?} overlaps the label before it");
                prop_assert_eq!(l.color, color);
                room_until = x + label_width(text);
                labels.next();
            }
            _ => prop_assert!(x < room_until, "no label for {text} at {x}"),
        }
    }
    prop_assert!(labels.next().is_none(), "labels left over");
    Ok(())
}

/// A random annotation: onset in the recording, an event, a sliver or a band, one of 14
/// descriptions (so colors repeat), for all channels or a few, or a stim annotation of a
/// channel.
fn annotation() -> impl Strategy<Value = AnnotationData> {
    (
        0.0..600.0f64,
        prop_oneof![Just(0.0), 0.0..0.01f64, 0.0..30.0f64],
        0..14usize,
        prop_oneof![
            2 => Just(vec![]),
            1 => proptest::collection::vec(0..8u32, 1..3),
        ],
        0..8u32,
        prop_oneof![3 => Just(false), 1 => Just(true)],
    )
        .prop_map(|(onset, duration, d, channels, stim_channel, stim)| {
            if stim {
                AnnotationData {
                    origin: Origin::Stim,
                    ..data(onset, 0.0, &format!("d{d}"), &[stim_channel])
                }
            } else {
                data(onset, duration, &format!("d{d}"), &channels)
            }
        })
}

/// A view moved, zoomed and scrolled at random, with some channels hidden.
fn view(rec: &Recording) -> impl Strategy<Value = ViewState> + use<> {
    let rec_view = ViewState::new(rec);
    (
        0.0..600.0f64,
        prop_oneof![Just(1.0), 0.001..1.0f64, 1.0..60.0f64],
        0.0..8.0f64,
        1..8usize,
        proptest::collection::vec(any::<bool>(), 8),
    )
        .prop_map(move |(start, zoom, top, rows, hidden)| {
            let mut view = rec_view.clone();
            for (c, hide) in hidden.into_iter().enumerate() {
                if hide {
                    view.set_shown(ChannelId(c as u32), false);
                }
            }
            view.zoom(zoom, 0.0);
            view.pan(start - view.start());
            view.set_rows(rows);
            view.set_top(top);
            view
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every pixel column a shown annotation touches holds a mark of its color over its
    /// rows, and no other column does; marks of a color and span never overlap.
    #[test]
    fn marks_cover_exactly_the_annotations_in_view(
        view in view(&recording()),
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..700u32,
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store, false);
        for d in hidden {
            legend.set_shown(None, &format!("d{d}"), false);
        }
        let marks = marks(&view, &store, &legend, width, label_width);
        check(&marks, &brute_force(&view, &store, &legend, width), width)?;
        check_labels(&marks, &view, &store, &legend, width)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The annotations under the pointer are exactly the shown ones with a mark within
    /// `slack` columns of it, over the pointer's row or the whole height, by onset.
    #[test]
    fn the_pointer_finds_the_marks_under_it(
        view in view(&recording()),
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..700u32,
        x in 0.0..1.0f64,
        slack in 0..4u32,
        channel in 0..9u32,
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store, false);
        for d in hidden {
            legend.set_shown(None, &format!("d{d}"), false);
        }
        let x = x * f64::from(width);
        // A channel on screen, as under the pointer, or none.
        let channel = Some(ChannelId(channel)).filter(|&c| {
            view.row_of(c).is_some_and(|r| view.visible_rows().contains(&r))
        });
        let at = (x.floor() as u32).min(width - 1);
        let near = at.saturating_sub(slack)..at + 1 + slack;
        let want: Vec<_> = store
            .iter()
            .filter(|a| {
                let span = if a.data.channels.is_empty() || a.data.origin == Origin::Stim {
                    Span::All
                } else if let Some(c) = channel.filter(|c| a.data.channels.contains(c)) {
                    Span::Row(view.row_of(c).unwrap())
                } else {
                    return false;
                };
                let alone = AnnotationStore::from_data(vec![a.data.clone()]);
                let cover = brute_force(&view, &alone, &legend, width);
                cover.into_iter().any(|((_, s), (bands, lines))| {
                    s == span && bands.union(&lines).any(|c| near.contains(c))
                })
            })
            .map(|a| a.id)
            .collect();
        let got: Vec<_> = under(&view, &store, &legend, width, x, slack, channel)
            .into_iter()
            .map(|a| a.id)
            .collect();
        prop_assert_eq!(got, want);
    }
}

/// What `overview_marks` must cover: every shown annotation over the whole recording,
/// over the strip's height, or with `rows` over the rows of its shown channels.
fn overview_brute_force(
    store: &AnnotationStore,
    legend: &Legend,
    duration: f64,
    width: u32,
    rows: Option<&ViewState>,
) -> Coverage {
    let mut cover = Coverage::new();
    let w = f64::from(width);
    for a in store.iter() {
        let d = &a.data;
        let entry = legend.of(d).unwrap();
        if !entry.shown {
            continue;
        }
        let spans: Vec<Span> = match rows {
            Some(view) if !d.channels.is_empty() && d.origin != Origin::Stim => d
                .channels
                .iter()
                .filter_map(|&c| view.row_of(c))
                .map(Span::Row)
                .collect(),
            _ => vec![Span::All],
        };
        let x0 = d.onset / duration * w;
        let x1 = d.end() / duration * w;
        let first = x0.floor().clamp(0.0, w - 1.0) as u32;
        for span in spans {
            let (bands, lines) = cover.entry((entry.color, span)).or_default();
            if x1 - x0 < 1.0 {
                lines.insert(first);
            } else {
                let end = (x1.ceil().min(w) as u32).max(first + 1);
                bands.extend(first..end);
            }
        }
    }
    cover
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The minimap's marks: every pixel column a shown annotation touches anywhere in the
    /// recording holds a mark of its color over the whole height, or, where the minimap
    /// has rows, over the rows of its shown channels (stim annotations over the height); no
    /// other column does, and there are no labels.
    #[test]
    fn overview_marks_cover_exactly_the_shown_annotations(
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..2000u32,
        rows in proptest::option::of(view(&recording())),
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store, false);
        for d in hidden {
            legend.set_shown(None, &format!("d{d}"), false);
        }
        let marks = overview_marks(&store, &legend, 600.0, width, rows.as_ref());
        let want = overview_brute_force(&store, &legend, 600.0, width, rows.as_ref());
        check(&marks, &want, width)?;
        prop_assert!(marks.labels.is_empty());
    }

    /// The annotations under the pointer over the minimap are exactly the shown ones with
    /// a mark within `slack` columns of it, by onset; where the minimap has rows, a mark
    /// over the height or the pointer's row.
    #[test]
    fn the_pointer_finds_the_overview_marks_under_it(
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..2000u32,
        x in 0.0..1.0f64,
        slack in 0..4u32,
        rows in proptest::option::of((view(&recording()), 0..8u32)),
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store, false);
        for d in hidden {
            legend.set_shown(None, &format!("d{d}"), false);
        }
        // With rows, the pointer is over a shown channel's.
        let (rows, channel) = match rows {
            Some((view, c)) => {
                let c = ChannelId(c);
                prop_assume!(view.row_of(c).is_some());
                (Some(view), Some(c))
            }
            None => (None, None),
        };
        let x = x * f64::from(width);
        let at = (x.floor() as u32).min(width - 1);
        let near = at.saturating_sub(slack)..at + 1 + slack;
        let want: Vec<_> = store
            .iter()
            .filter(|a| {
                let alone = AnnotationStore::from_data(vec![a.data.clone()]);
                let pointed = channel.and_then(|c| rows.as_ref()?.row_of(c)).map(Span::Row);
                overview_brute_force(&alone, &legend, 600.0, width, rows.as_ref())
                    .into_iter()
                    .filter(|((_, span), _)| pointed.is_none() || *span == Span::All || Some(*span) == pointed)
                    .any(|(_, (bands, lines))| bands.union(&lines).any(|c| near.contains(c)))
            })
            .map(|a| a.id)
            .collect();
        let got: Vec<_> = overview_under(&store, &legend, 600.0, width, x, slack, channel)
            .into_iter()
            .map(|a| a.id)
            .collect();
        prop_assert_eq!(got, want);
    }
}

/// Picking a color for a description counts as a change of the legend's colors, setting
/// the same colors again doesn't.
#[test]
fn picked_colors_count_as_changes() {
    let mut store = AnnotationStore::default();
    insert(&mut store, data(1.0, 0.0, "a", &[]));
    let mut legend = Legend::new(&store, false);
    let before = legend.colors_revision();
    let mut picked = kymo::annotations::AnnotationColors::new();
    legend.apply_colors(&picked);
    assert_eq!(legend.colors_revision(), before);
    picked.insert("a".into(), [1, 2, 3]);
    legend.apply_colors(&picked);
    let after = legend.colors_revision();
    assert!(after > before);
    legend.apply_colors(&picked);
    assert_eq!(legend.colors_revision(), after);
}

/// However many annotations there are, there are at most a few marks per pixel column.
#[test]
fn thousands_of_events_thin_to_the_screen() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.zoom(view.span() / 600.0, 0.0);
    assert_eq!(view.time(), 0.0..600.0);
    let annotations = (0..20_000)
        .map(|k| data(k as f64 * 0.03, 0.0, &format!("event {k}"), &[]))
        .collect();
    let store = AnnotationStore::from_data(annotations);
    let legend = Legend::new(&store, false);
    let marks = marks(&view, &store, &legend, 400, label_width);
    assert!(marks.bands.is_empty());
    assert!(marks.lines.len() <= 400 * COLORS, "{}", marks.lines.len());
    assert!(marks.lines.len() >= 400);
    assert!(marks.labels.len() <= 400);
}

/// Events tied to channels mark only those rows; one whose channels are all off screen draws
/// nothing.
#[test]
fn channel_annotations_cover_their_rows() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.set_rows(4);
    view.set_shown(ChannelId(1), false);
    // Rows: 0, 2, 3, 4 on screen; 5, 6, 7 below.
    let store = AnnotationStore::from_data(vec![
        data(1.0, 2.0, "a", &[2, 4]),
        data(1.0, 2.0, "b", &[1, 6]),
        data(5.0, 0.0, "c", &[0]),
    ]);
    let legend = Legend::new(&store, false);
    let marks = marks(&view, &store, &legend, 200, label_width);
    let spans = |marks: &[Mark]| -> Vec<(AnnotationColor, Span)> {
        let mut s: Vec<_> = marks.iter().map(|m| (m.color, m.span)).collect();
        s.sort();
        s
    };
    assert_eq!(
        spans(&marks.bands),
        [(Palette(0), Span::Row(1)), (Palette(0), Span::Row(3))]
    );
    assert_eq!(spans(&marks.lines), [(Palette(2), Span::Row(0))]);
    let texts: Vec<_> = marks.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["a", "c"]);

    // A row's span is that row, a quarter of the height; all channels span all of it.
    let height = 400.0;
    for (row, channel) in [(0, 0), (1, 2), (3, 4)] {
        let y = span_y(&view, Span::Row(row), height);
        assert!((y.end - y.start - 100.0).abs() < 1e-9, "{y:?}");
        for at in [y.start + 0.5, (y.start + y.end) / 2.0, y.end - 0.5] {
            assert_eq!(view.channel_at(at, height), Some(ChannelId(channel)));
        }
    }
    assert_eq!(span_y(&view, Span::All, height), 0.0..height);
}

/// A stim annotation belongs to its stim channel but marks all rows, even with that channel off
/// screen or hidden, and the pointer finds it on any of them.
#[test]
fn stim_annotations_mark_every_row() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.set_rows(4);
    view.set_shown(ChannelId(1), false);
    let stim = |onset: f64, channel: u32| AnnotationData {
        origin: Origin::Stim,
        ..data(onset, 0.0, "7", &[channel])
    };
    let store = AnnotationStore::from_data(vec![stim(5.0, 6), stim(10.0, 1)]);
    let legend = Legend::new(&store, false);
    let marks = marks(&view, &store, &legend, 200, label_width);
    let lines: Vec<_> = marks
        .lines
        .iter()
        .map(|m| (m.columns.start, m.span))
        .collect();
    assert_eq!(lines, [(50, Span::All), (100, Span::All)]);
    assert_eq!(marks.labels.len(), 2);
    for channel in [None, Some(ChannelId(0)), Some(ChannelId(4))] {
        let found = under(&view, &store, &legend, 200, 100.5, 0, channel);
        assert_eq!(found.len(), 1, "{channel:?}");
        assert_eq!(found[0].data.onset, 10.0);
    }
}

/// Translucent bands of different colors that overlap blend by the order they are drawn
/// in, so that order must never change from one frame to the next: by color, then rows,
/// then from left to right.
#[test]
fn bands_come_in_a_fixed_order() {
    let rec = recording();
    let view = ViewState::new(&rec);
    let mut items: Vec<AnnotationData> = (0..6)
        .map(|k| data(f64::from(k), 30.0, &format!("d{k}"), &[]))
        .collect();
    items.push(data(0.5, 1.0, "d3", &[]));
    items.push(data(2.0, 3.0, "d1", &[0]));
    let store = AnnotationStore::from_data(items);
    let legend = Legend::new(&store, false);
    let first = marks(&view, &store, &legend, 300, label_width);
    let keys: Vec<_> = first
        .bands
        .iter()
        .map(|m| (m.color, m.span, m.columns.start))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    for _ in 0..50 {
        assert_eq!(marks(&view, &store, &legend, 300, label_width), first);
    }
}

#[test]
fn a_press_grabs_the_edges_within_reach_and_the_body_between() {
    use kymo::annotations::Grip::{Body, End, Start};
    let wide = |x| kymo::annotations::grip(0.0, 100.0, x, 4.0, 0.5);
    let found: Vec<_> = [-4.5, -4.0, 0.0, 4.0, 4.5, 50.0, 95.5, 96.0, 104.0, 104.5]
        .map(wide)
        .into();
    assert_eq!(
        found,
        [
            None,
            Some(Start),
            Some(Start),
            Some(Start),
            Some(Body),
            Some(Body),
            Some(Body),
            Some(End),
            Some(End),
            None
        ]
    );
    // Short: the edges reach a third of the way in, leaving the middle to move by.
    let short = |x| kymo::annotations::grip(0.0, 6.0, x, 4.0, 0.5);
    assert_eq!(short(2.0), Some(Start));
    assert_eq!(short(3.0), Some(Body));
    assert_eq!(short(4.0), Some(End));
    assert_eq!(short(-4.0), Some(Start));
    // Narrower than a line: moved only, within reach of its middle.
    let line = |x| kymo::annotations::grip(10.0, 10.2, x, 4.0, 0.5);
    assert_eq!(line(10.1), Some(Body));
    assert_eq!(line(5.85), Some(Body));
    assert_eq!(line(14.35), Some(Body));
    assert_eq!(line(5.8), None);
    assert_eq!(line(14.4), None);
}

/// The closed intervals `items` cover, joined where they overlap or touch.
fn union(mut items: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (start, end) in items {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// Annotations on a grid of quarter seconds, so ends that touch are exactly equal; few
/// descriptions, channel sets and streams, so many are of a kind.
fn any_mergeable() -> impl Strategy<Value = AnnotationData> {
    (
        0u8..40,
        0u8..12,
        prop_oneof![Just("BAD"), Just("x")],
        prop_oneof![
            Just(vec![]),
            Just(vec![1]),
            Just(vec![1, 2]),
            Just(vec![2, 1])
        ],
        prop_oneof![
            3 => Just(Origin::File),
            3 => Just(Origin::Viewer),
            1 => Just(Origin::Stim)
        ],
        prop_oneof![3 => Just(None), 1 => Just(Some(0)), 1 => Just(Some(1))],
    )
        .prop_map(
            |(onset, duration, description, channels, origin, stream)| AnnotationData {
                origin,
                stream,
                ..data(
                    f64::from(onset) / 4.0,
                    f64::from(duration) / 4.0,
                    description,
                    &channels,
                )
            },
        )
}

/// Whether `a` and `b` merge, without `annotations::same_kind`: the same description,
/// channels as sets and, `by_stream`, stream, neither from the stim channel.
fn of_a_kind(a: &AnnotationData, b: &AnnotationData, by_stream: bool) -> bool {
    let set = |d: &AnnotationData| d.channels.iter().copied().collect::<BTreeSet<_>>();
    a.origin != Origin::Stim
        && b.origin != Origin::Stim
        && a.description == b.description
        && set(a) == set(b)
        && (!by_stream || a.stream == b.stream)
}

proptest! {
    #[test]
    fn merging_joins_exactly_the_annotations_of_a_kind_it_reaches(
        items in proptest::collection::vec(any_mergeable(), 1..30),
        n: usize,
        by_stream: bool,
    ) {
        use kymo::annotations::merge;
        let store = AnnotationStore::from_data(items);
        let all: Vec<Annotation> = store.iter().cloned().collect();
        let edited = all[n % all.len()].clone();
        let changes = merge(&store, edited.id, by_stream);
        let mut after = store.clone();
        for (id, data) in &changes {
            kymo::history::set(&mut after, *id, data.clone()).unwrap();
        }
        if edited.data.origin == Origin::Stim {
            prop_assert!(changes.is_empty());
            return Ok(());
        }
        // The edited one grows; the rest of the changes remove annotations of its kind.
        if let Some((first, rest)) = changes.split_first() {
            prop_assert_eq!(first.0, edited.id);
            prop_assert!(first.1.is_some());
            for (id, data) in rest {
                prop_assert!(data.is_none());
                prop_assert!(of_a_kind(&edited.data, &store.get(*id).unwrap().data, by_stream));
            }
        }
        let grown = after.get(edited.id).unwrap().data.clone();
        prop_assert_eq!(
            AnnotationData { onset: 0.0, duration: 0.0, ..grown.clone() },
            AnnotationData { onset: 0.0, duration: 0.0, ..edited.data.clone() }
        );
        // It overlaps and touches nothing of its kind any more.
        for b in after.iter() {
            prop_assert!(
                b.id == edited.id
                    || !of_a_kind(&grown, &b.data, by_stream)
                    || b.data.onset > grown.end()
                    || b.data.end() < grown.onset,
                "{:?} still reaches {:?}", b, grown
            );
        }
        // Its kind covers the same time as before, and every other annotation is as it was.
        let covered = |s: &AnnotationStore| {
            union(
                s.iter()
                    .filter(|b| of_a_kind(&edited.data, &b.data, by_stream))
                    .map(|b| (b.data.onset, b.data.end()))
                    .collect(),
            )
        };
        prop_assert_eq!(covered(&after), covered(&store));
        let others = |s: &AnnotationStore| {
            s.iter()
                .filter(|b| !of_a_kind(&edited.data, &b.data, by_stream))
                .cloned()
                .collect::<Vec<_>>()
        };
        prop_assert_eq!(others(&after), others(&store));
    }
}

proptest! {
    #[test]
    fn merging_the_shown_leaves_none_of_a_kind_reaching_another(
        items in proptest::collection::vec(any_mergeable(), 0..40),
        bad_shown: bool,
        x_shown: bool,
        by_stream: bool,
    ) {
        use kymo::annotations::merge_shown;
        let shown = |d: &str| if d == "BAD" { bad_shown } else { x_shown };
        let store = AnnotationStore::from_data(items);
        let merged = merge_shown(&store, by_stream, |a| shown(&a.description));
        let mut after = store.clone();
        for m in &merged {
            kymo::history::set(&mut after, m.kept, Some(m.data.clone())).unwrap();
            for &id in &m.removed {
                kymo::history::set(&mut after, id, None).unwrap();
            }
        }
        let merges = |a: &AnnotationData| a.origin != Origin::Stim && shown(&a.description);

        // Afterward, no two shown annotations of a kind overlap or touch.
        let left: Vec<&Annotation> = after.iter().filter(|a| merges(&a.data)).collect();
        for (i, a) in left.iter().enumerate() {
            for b in &left[i + 1..] {
                prop_assert!(
                    !of_a_kind(&a.data, &b.data, by_stream)
                        || b.data.onset > a.data.end()
                        || b.data.end() < a.data.onset,
                    "{:?} still reaches {:?}", a, b
                );
            }
        }
        // Each kind covers the same time as before.
        for a in store.iter().filter(|a| merges(&a.data)) {
            let covered = |s: &AnnotationStore| {
                union(
                    s.iter()
                        .filter(|b| of_a_kind(&a.data, &b.data, by_stream))
                        .map(|b| (b.data.onset, b.data.end()))
                        .collect(),
                )
            };
            prop_assert_eq!(covered(&after), covered(&store));
        }
        // The earliest of a group is kept, its onset, description, channels and origin as
        // they were; the others were of its kind and are gone.
        for m in &merged {
            prop_assert!(!m.removed.is_empty());
            let kept = &store.get(m.kept).unwrap().data;
            prop_assert!(merges(kept));
            prop_assert_eq!(
                &AnnotationData { duration: 0.0, ..m.data.clone() },
                &AnnotationData { duration: 0.0, ..kept.clone() }
            );
            for id in &m.removed {
                let b = &store.get(*id).unwrap().data;
                prop_assert!(of_a_kind(kept, b, by_stream));
                prop_assert!((b.onset, id.0) > (kept.onset, m.kept.0));
                prop_assert!(after.get(*id).is_none());
            }
        }
        // Everything else is as it was: hidden and stim annotations, and those that reached
        // nothing.
        let changed: BTreeSet<_> = merged
            .iter()
            .flat_map(|m| std::iter::once(m.kept).chain(m.removed.iter().copied()))
            .collect();
        for a in store.iter().filter(|a| !changed.contains(&a.id)) {
            prop_assert_eq!(after.get(a.id), Some(a));
        }
    }
}

#[test]
fn merging_the_shown_keeps_a_covering_annotation_exact() {
    // 0.1 + 0.2 - 0.1 isn't 0.2 in floating point: a duration recomputed from the end
    // would change the annotation that covers the other.
    let store = AnnotationStore::from_data(vec![
        data(0.1, 0.2, "BAD", &[]),
        data(0.15, 0.05, "BAD", &[]),
    ]);
    let merged = kymo::annotations::merge_shown(&store, false, |_| true);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].data, store.get(merged[0].kept).unwrap().data);
}
