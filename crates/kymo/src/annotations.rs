//! The annotation overlay: the legend (which descriptions are shown, in which color) and the
//! marks drawn for the annotations in view, thinned to what the screen can show.
//!
//! Marks are in physical pixel columns of the trace area and in rows, so this module needs
//! no GUI; `viewer` turns them into shapes.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use kymo_core::{
    Annotation, AnnotationData, AnnotationId, AnnotationStore, ChannelId, ChannelKind, Origin,
    Recording, StreamContent,
};

use crate::view::ViewState;

/// Colors annotations cycle through (see `ui::palette::annotation_color`).
pub const COLORS: usize = 10;

/// A color as red, green and blue (sRGB).
pub type Rgb = [u8; 3];

/// Colors picked for descriptions, remembered across recordings.
pub type AnnotationColors = BTreeMap<String, Rgb>;

/// The color of a description's marks: a palette color (lighter in dark themes), or one
/// picked for it, used as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnnotationColor {
    /// Index into the annotation colors, `< COLORS`.
    Palette(usize),
    Custom(Rgb),
}

/// One description in the legend, of one stream where the legend is grouped by stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The stream its annotations come from (see [`AnnotationData::stream`]) where the
    /// legend is grouped by stream, else `None`.
    pub stream: Option<usize>,
    pub description: String,
    /// Its palette color, by first appearance, `< COLORS`; kept while a picked color
    /// replaces it.
    pub slot: usize,
    /// The palette color, or the one picked for its description (see
    /// [`Legend::apply_colors`]).
    pub color: AnnotationColor,
    /// Annotations with this description (and stream) in the store.
    pub count: usize,
    pub shown: bool,
}

/// The descriptions of a store's annotations, each with a color that never changes, a
/// count, and whether it is shown. Grouped by stream (where a recording has more than one
/// stream with annotations, see [`groups_streams`]), a description has an entry per
/// stream it comes from; otherwise one, whatever the streams.
#[derive(Clone, Debug, Default)]
pub struct Legend {
    /// In order of first appearance. Entries are never removed, so their colors (by
    /// position) never change.
    entries: Vec<Entry>,
    /// Position of each entry, by stream, then description.
    index: HashMap<Option<usize>, HashMap<String, usize>>,
    by_stream: bool,
    /// Counts the changes of what is shown, for [`shown_revision`](Self::shown_revision).
    shown_revision: u64,
    /// Counts the changes of colors, for [`colors_revision`](Self::colors_revision).
    colors_revision: u64,
}

/// Whether the legend of `recording` groups its descriptions by stream: where more than
/// one stream holds annotations (an XDF stream of annotations, one with a stim channel
/// whose changes are events, or one the file's annotations in `store` belong to, as a BVRF
/// participant's markers do). This depends on the file only (annotations added in the
/// viewer have no stream), so adding annotations never regroups the legend.
pub fn groups_streams(recording: &Recording, store: &AnnotationStore) -> bool {
    let mut holds: Vec<bool> = recording
        .streams()
        .iter()
        .map(|s| match &s.content {
            StreamContent::Annotations(_) => true,
            StreamContent::Channels(ids) => ids.iter().any(|&c| {
                let channel = recording.channel(c);
                channel.kind == ChannelKind::STIM && channel.events
            }),
            StreamContent::NotShown(_) => false,
        })
        .collect();
    for a in store.iter() {
        if a.data.origin == Origin::File
            && let Some(held) = a.data.stream.and_then(|s| holds.get_mut(s))
        {
            *held = true;
        }
    }
    holds.into_iter().filter(|&h| h).count() > 1
}

impl Legend {
    /// The legend of `store`, everything shown; grouped by stream with `by_stream` (see
    /// [`groups_streams`]).
    pub fn new(store: &AnnotationStore, by_stream: bool) -> Legend {
        let mut legend = Legend {
            by_stream,
            ..Legend::default()
        };
        legend.sync(store);
        legend
    }

    /// Whether descriptions are grouped by stream.
    pub fn by_stream(&self) -> bool {
        self.by_stream
    }

    /// The stream `a` is listed under: its own where grouped by stream, else `None`.
    pub fn stream_of(&self, a: &AnnotationData) -> Option<usize> {
        if self.by_stream { a.stream } else { None }
    }

    /// Counts `store`'s annotations per entry. Entries not seen before are added at the
    /// end, by onset, shown, each with the next color; known ones keep their color and
    /// shown state, even while none of their annotations are left.
    pub fn sync(&mut self, store: &AnnotationStore) {
        for entry in &mut self.entries {
            entry.count = 0;
        }
        for a in store.iter() {
            let stream = self.stream_of(&a.data);
            let description = &a.data.description;
            let i = match self.position(stream, description) {
                Some(i) => i,
                None => {
                    let i = self.entries.len();
                    self.entries.push(Entry {
                        stream,
                        description: description.clone(),
                        slot: i % COLORS,
                        color: AnnotationColor::Palette(i % COLORS),
                        count: 0,
                        shown: true,
                    });
                    self.index
                        .entry(stream)
                        .or_default()
                        .insert(description.clone(), i);
                    i
                }
            };
            self.entries[i].count += 1;
        }
    }

    fn position(&self, stream: Option<usize>, description: &str) -> Option<usize> {
        self.index.get(&stream)?.get(description).copied()
    }

    /// Colors each entry as picked for its description in `picked`, or with its palette
    /// color.
    pub fn apply_colors(&mut self, picked: &AnnotationColors) {
        for entry in &mut self.entries {
            let color = picked
                .get(&entry.description)
                .map_or(AnnotationColor::Palette(entry.slot), |&rgb| {
                    AnnotationColor::Custom(rgb)
                });
            if entry.color != color {
                entry.color = color;
                self.colors_revision += 1;
            }
        }
    }

    /// A number that grows whenever an entry's color changes. Entries new to the legend
    /// get theirs without it, as they come with a change of the store.
    pub fn colors_revision(&self) -> u64 {
        self.colors_revision
    }

    /// Entries with at least one annotation, in order of first appearance.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> + '_ {
        self.entries.iter().filter(|e| e.count > 0)
    }

    /// The streams of the entries with annotations, in the recording's order, `None`
    /// (annotations without a stream, or every one if not grouped) last.
    pub fn streams(&self) -> Vec<Option<usize>> {
        let mut streams: Vec<Option<usize>> = self.entries().map(|e| e.stream).collect();
        streams.sort_by_key(|s| (s.is_none(), *s));
        streams.dedup();
        streams
    }

    /// The entry of `description` listed under `stream` (see [`stream_of`](Self::stream_of)).
    pub fn entry(&self, stream: Option<usize>, description: &str) -> Option<&Entry> {
        self.position(stream, description).map(|i| &self.entries[i])
    }

    /// The entry `a` is listed under.
    pub fn of(&self, a: &AnnotationData) -> Option<&Entry> {
        self.entry(self.stream_of(a), &a.description)
    }

    /// Whether `a`'s description (and stream) is shown.
    pub fn shows(&self, a: &AnnotationData) -> bool {
        self.of(a).is_some_and(|e| e.shown)
    }

    /// Whether there is no annotation to show.
    pub fn is_empty(&self) -> bool {
        self.entries().next().is_none()
    }

    pub fn set_shown(&mut self, stream: Option<usize>, description: &str, shown: bool) {
        if let Some(i) = self.position(stream, description)
            && self.entries[i].shown != shown
        {
            self.entries[i].shown = shown;
            self.shown_revision += 1;
        }
    }

    /// Shows or hides every entry of `stream`.
    pub fn set_stream_shown(&mut self, stream: Option<usize>, shown: bool) {
        for entry in &mut self.entries {
            if entry.stream == stream {
                entry.shown = shown;
            }
        }
        self.shown_revision += 1;
    }

    pub fn set_all_shown(&mut self, shown: bool) {
        for entry in &mut self.entries {
            entry.shown = shown;
        }
        self.shown_revision += 1;
    }

    /// A number that grows whenever a description is shown or hidden. Entries new to the
    /// legend are shown without it, as they come with a change of the store.
    pub fn shown_revision(&self) -> u64 {
        self.shown_revision
    }

    /// How many entries with annotations are shown, and how many there are.
    pub fn shown_count(&self) -> (usize, usize) {
        Self::count_shown(self.entries())
    }

    /// How many entries of `stream` with annotations are shown, and how many there are.
    pub fn stream_shown(&self, stream: Option<usize>) -> (usize, usize) {
        Self::count_shown(self.entries().filter(|e| e.stream == stream))
    }

    fn count_shown<'e>(entries: impl Iterator<Item = &'e Entry>) -> (usize, usize) {
        entries.fold((0, 0), |(shown, all), e| {
            (shown + usize::from(e.shown), all + 1)
        })
    }

    /// How many annotations there are, of every description.
    pub fn total(&self) -> usize {
        self.entries().map(|e| e.count).sum()
    }

    /// How many annotations of `stream` there are.
    pub fn stream_total(&self, stream: Option<usize>) -> usize {
        self.entries()
            .filter(|e| e.stream == stream)
            .map(|e| e.count)
            .sum()
    }
}

/// The rows a mark covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Span {
    /// The whole height of the trace area: an annotation of all channels.
    All,
    /// One row (an index into the view's rows), for an annotation tied to its channel.
    Row(usize),
}

/// A band or line of one color over whole pixel columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Physical pixel columns of the trace area, never empty.
    pub columns: Range<u32>,
    pub span: Span,
    pub color: AnnotationColor,
}

/// A description at the top of the trace area.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// Physical pixels from the left edge of the trace area: the annotation's start, or 0
    /// if it starts before the view.
    pub x: f64,
    pub text: String,
    pub color: AnnotationColor,
}

/// What to draw for the annotations in view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Translucent: annotations at least a pixel long. Bands of the same color and span
    /// never overlap or touch; overlapping ones are merged. By color, span and first
    /// column, so bands of different colors that overlap blend the same in every frame.
    pub bands: Vec<Mark>,
    /// Solid, one column wide: annotations without or with less than a pixel's duration. At most one
    /// per column, color and span.
    pub lines: Vec<Mark>,
    /// From left to right, never overlapping.
    pub labels: Vec<Label>,
}

/// The marks for `store`'s annotations in `view`, on a trace area `width_px` physical
/// pixels wide, of the descriptions `legend` shows.
///
/// An annotation of all channels, or of a stim channel, spans the trace area's height (see
/// [`spans_all`]); one tied to channels spans the rows of those on screen, and draws nothing
/// if none is. Marks of the same color over
/// the same rows that would overlap are merged, so however many annotations there are,
/// there are at most a few marks per pixel column, and none is lost: every pixel an
/// annotation covers is covered by a mark of its color.
///
/// Each annotation drawn gets a label unless it would overlap the previous one;
/// `label_width` gives the room a label takes, in pixels, the gap to the next included.
pub fn marks(
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width_px: u32,
    mut label_width: impl FnMut(&str) -> f64,
) -> Marks {
    if width_px == 0 {
        return Marks::default();
    }
    let rows = view.visible_rows();
    let time = view.time();
    let mut merger = Merger::default();
    let mut next_label = f64::NEG_INFINITY;
    let mut spans = Vec::new();
    // By onset, so marks of each color and span come from left to right.
    for a in store.query(time.clone()) {
        let Some(entry) = legend.of(&a.data).filter(|e| e.shown) else {
            continue;
        };
        spans.clear();
        if spans_all(&a.data) {
            spans.push(Span::All);
        } else {
            spans.extend(
                a.data
                    .channels
                    .iter()
                    .filter_map(|&c| view.row_of(c))
                    .filter(|r| rows.contains(r))
                    .map(Span::Row),
            );
            spans.sort();
            spans.dedup();
        }
        if spans.is_empty() {
            continue;
        }

        let (columns, line) = columns(view.start(), view.span(), &a.data, width_px);
        for &span in &spans {
            merger.add(entry.color, span, &columns, line);
        }

        let x = view.x_at(a.data.onset, f64::from(width_px)).max(0.0);
        if x >= next_label {
            next_label = x + label_width(&a.data.description);
            merger.marks.labels.push(Label {
                x,
                text: a.data.description.clone(),
                color: entry.color,
            });
        }
    }
    merger.finish()
}

/// The marks for all of `store`'s annotations over a recording `duration` seconds long, on
/// a strip `width_px` physical pixels wide (the minimap), of the descriptions `legend`
/// shows: as [`marks`] makes them, but over the whole recording and without labels. Each
/// spans the strip's height, unless the strip has `rows`, the view's, all of them: then an
/// annotation tied to channels spans the rows of those shown, and draws nothing if none is.
pub fn overview_marks(
    store: &AnnotationStore,
    legend: &Legend,
    duration: f64,
    width_px: u32,
    rows: Option<&ViewState>,
) -> Marks {
    let mut merger = Merger::default();
    if width_px == 0 {
        return merger.marks;
    }
    let mut spans = Vec::new();
    for a in store.iter() {
        let Some(entry) = legend.of(&a.data).filter(|e| e.shown) else {
            continue;
        };
        spans.clear();
        match rows {
            Some(view) if !spans_all(&a.data) => {
                spans.extend(
                    a.data
                        .channels
                        .iter()
                        .filter_map(|&c| view.row_of(c))
                        .map(Span::Row),
                );
                spans.sort();
                spans.dedup();
            }
            _ => spans.push(Span::All),
        }
        let (columns, line) = columns(0.0, duration, &a.data, width_px);
        for &span in &spans {
            merger.add(entry.color, span, &columns, line);
        }
    }
    merger.finish()
}

/// Marks of one color and span merged as they come, from left to right.
#[derive(Default)]
struct Merger {
    /// The band being extended and the last line, per color and span.
    open: HashMap<(AnnotationColor, Span), Range<u32>>,
    lines: HashMap<(AnnotationColor, Span), u32>,
    marks: Marks,
}

impl Merger {
    /// Adds a mark over `columns`, a line or a band; marks of a color and span must come
    /// by their first column.
    fn add(&mut self, color: AnnotationColor, span: Span, columns: &Range<u32>, line: bool) {
        let key = (color, span);
        if line {
            if self.lines.insert(key, columns.start) != Some(columns.start) {
                self.marks.lines.push(Mark {
                    columns: columns.clone(),
                    span,
                    color,
                });
            }
        } else {
            let columns = columns.clone();
            match self.open.get_mut(&key) {
                Some(band) if columns.start <= band.end => band.end = band.end.max(columns.end),
                Some(band) => self.marks.bands.push(Mark {
                    columns: std::mem::replace(band, columns),
                    span,
                    color,
                }),
                None => {
                    self.open.insert(key, columns);
                }
            }
        }
    }

    fn finish(self) -> Marks {
        let mut marks = self.marks;
        marks
            .bands
            .extend(self.open.into_iter().map(|((color, span), columns)| Mark {
                columns,
                span,
                color,
            }));
        // The bands still open come in the hash map's order, which changes with every map.
        marks
            .bands
            .sort_unstable_by_key(|m| (m.color, m.span, m.columns.start));
        marks
    }
}

/// The annotations under pixel column `x` of a trace area `width_px` physical pixels wide,
/// by onset: those of the descriptions `legend` shows whose mark is within `slack` columns
/// of `x`, and that span all rows or apply to `channel`, the one under the pointer.
pub fn under<'s>(
    view: &ViewState,
    store: &'s AnnotationStore,
    legend: &Legend,
    width_px: u32,
    x: f64,
    slack: u32,
    channel: Option<ChannelId>,
) -> Vec<&'s Annotation> {
    let applies =
        |a: &AnnotationData| spans_all(a) || channel.is_some_and(|c| a.channels.contains(&c));
    near(
        store,
        legend,
        (view.start(), view.span()),
        width_px,
        x,
        slack,
        applies,
    )
}

/// The annotations under pixel column `x` of the minimap, `width_px` physical pixels wide
/// over a recording `duration` seconds long, by onset: those of the descriptions `legend`
/// shows whose mark is within `slack` columns of `x` (see [`overview_marks`]); where the
/// minimap has rows, only those that span them all or apply to `channel`, the one under
/// the pointer.
pub fn overview_under<'s>(
    store: &'s AnnotationStore,
    legend: &Legend,
    duration: f64,
    width_px: u32,
    x: f64,
    slack: u32,
    channel: Option<ChannelId>,
) -> Vec<&'s Annotation> {
    let applies =
        |a: &AnnotationData| channel.is_none_or(|c| spans_all(a) || a.channels.contains(&c));
    near(store, legend, (0.0, duration), width_px, x, slack, applies)
}

/// The annotations `applies` takes whose mark is within `slack` columns of column `x`, on
/// `width_px` columns showing `span` seconds from `start`.
fn near<'s>(
    store: &'s AnnotationStore,
    legend: &Legend,
    (start, span): (f64, f64),
    width_px: u32,
    x: f64,
    slack: u32,
    applies: impl Fn(&AnnotationData) -> bool,
) -> Vec<&'s Annotation> {
    if width_px == 0 {
        return Vec::new();
    }
    let at = column(x, width_px - 1);
    let width = f64::from(width_px);
    // Within the trace area: annotations beyond it are not drawn.
    let near = at.saturating_sub(slack)..(at + 1 + slack).min(width_px);
    let time_at = |column: u32| start + f64::from(column) / width * span;
    store
        .query(time_at(near.start)..time_at(near.end))
        .filter(|a| legend.shows(&a.data))
        .filter(|a| applies(&a.data))
        .filter(|a| {
            let (columns, _) = columns(start, span, &a.data, width_px);
            columns.start < near.end && near.start < columns.end
        })
        .collect()
}

/// The annotation a click at pixel column `x` selects, of the ones [`under`] finds that can
/// be edited (stim annotations can't): the shortest, so events and short annotations
/// inside longer ones can be reached; on `current`, the one selected, the next longer one
/// there, and after the longest the shortest again. So repeated clicks reach every
/// annotation under the pointer, even one lying wholly over shorter ones. Equally long
/// ones go by onset.
#[expect(clippy::too_many_arguments)]
pub fn pick(
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width_px: u32,
    x: f64,
    slack: u32,
    channel: Option<ChannelId>,
    current: Option<AnnotationId>,
) -> Option<AnnotationId> {
    let mut found: Vec<&Annotation> = under(view, store, legend, width_px, x, slack, channel)
        .into_iter()
        .filter(|a| a.data.origin != Origin::Stim)
        .collect();
    // Stable, so equally long ones stay in `under`'s order.
    found.sort_by(|a, b| a.data.duration.total_cmp(&b.data.duration));
    let next = current
        .and_then(|c| found.iter().position(|a| a.id == c))
        .map_or(0, |i| (i + 1) % found.len());
    found.get(next).map(|a| a.id)
}

/// The pixel columns `a`'s mark covers on a trace area `width_px` pixels wide, if it is in
/// `view`'s window.
pub fn extent(view: &ViewState, a: &AnnotationData, width_px: u32) -> Option<Range<u32>> {
    let time = view.time();
    (width_px > 0 && a.onset < time.end && a.end() >= time.start)
        .then(|| columns(view.start(), view.span(), a, width_px).0)
}

/// Whether `a`'s mark spans the trace area's height: if it applies to all channels, or is a
/// stim annotation. Those belong to their stim channel, but mark a moment for all channels, as
/// MNE draws them; and the stim row may well be off screen.
pub fn spans_all(a: &AnnotationData) -> bool {
    a.channels.is_empty() || a.origin == Origin::Stim
}

/// The pixel columns `a`'s mark covers on a trace area `width_px` (`> 0`) pixels wide that
/// shows `span` seconds from `start`, and whether it is a line: one column, for annotations shorter than a pixel.
fn columns(start: f64, span: f64, a: &AnnotationData, width_px: u32) -> (Range<u32>, bool) {
    let width = f64::from(width_px);
    // As `ViewState::x_at`, to the bit.
    let x_at = |t: f64| (t - start) / span * width;
    let x0 = x_at(a.onset);
    let x1 = x_at(a.end());
    let first = column(x0, width_px - 1);
    if x1 - x0 < 1.0 {
        (first..first + 1, true)
    } else {
        (first..(x1.ceil().min(width) as u32).max(first + 1), false)
    }
}

/// The y range `span` covers in a trace area `height` tall, from its top, in the units of
/// `height`.
pub fn span_y(view: &ViewState, span: Span, height: f64) -> Range<f64> {
    match span {
        Span::All => 0.0..height,
        Span::Row(row) => {
            view.y_of_row(row as f64, height)..view.y_of_row(row as f64 + 1.0, height)
        }
    }
}

/// The pixel column holding `x`, within `0..=last`.
fn column(x: f64, last: u32) -> u32 {
    x.floor().clamp(0.0, f64::from(last)) as u32
}

/// The part of an annotation a press grabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    /// Its onset, to resize it.
    Start,
    /// Its end, to resize it.
    End,
    /// The rest, to move it.
    Body,
}

/// What a press at `x` grabs of an annotation drawn from `left` to `right` (any unit, say
/// points): within `edge` of an edge, that edge, except that the edges reach at most a
/// third of the way in, so a short annotation keeps a middle to move it by; inside, the
/// body. An annotation narrower than `line` is a line, moved only, grabbed within `edge`.
pub fn grip(left: f64, right: f64, x: f64, edge: f64, line: f64) -> Option<Grip> {
    if right - left < line {
        let center = (left + right) / 2.0;
        return ((x - center).abs() <= edge + line / 2.0).then_some(Grip::Body);
    }
    let inner = edge.min((right - left) / 3.0);
    if x < left - edge || x > right + edge {
        None
    } else if x <= left + inner {
        Some(Grip::Start)
    } else if x >= right - inner {
        Some(Grip::End)
    } else {
        Some(Grip::Body)
    }
}

/// Whether `a` and `b` are of a kind that merges: the same description and channels, with
/// `by_stream` (where the legend groups by stream, see [`Legend::by_stream`]) the same
/// stream, neither a stim annotation.
pub fn same_kind(a: &AnnotationData, b: &AnnotationData, by_stream: bool) -> bool {
    let sorted = |c: &[ChannelId]| {
        let mut c = c.to_vec();
        c.sort();
        c.dedup();
        c
    };
    a.origin != Origin::Stim
        && b.origin != Origin::Stim
        && a.description == b.description
        && sorted(&a.channels) == sorted(&b.channels)
        && (!by_stream || a.stream == b.stream)
}

/// The changes that merge annotation `id` with every annotation of its kind (see
/// [`same_kind`]) that overlaps or touches it, and then with those that overlap or touch
/// the result, until none does: `id` grows to cover them all and the others are removed.
/// Empty if there is nothing to merge, `id` isn't there or is a stim annotation.
pub fn merge(
    store: &AnnotationStore,
    id: AnnotationId,
    by_stream: bool,
) -> Vec<(AnnotationId, Option<AnnotationData>)> {
    let Some(edited) = store.get(id) else {
        return Vec::new();
    };
    let mut data = edited.data.clone();
    let (mut start, mut end) = (data.onset, data.end());
    let mut merged: Vec<AnnotationId> = Vec::new();
    loop {
        // Closed intervals: an annotation that ends at `start` or starts at `end` touches.
        let found: Vec<&Annotation> = store
            .query(start..end.next_up())
            .filter(|b| {
                b.id != id
                    && !merged.contains(&b.id)
                    && b.data.onset <= end
                    && b.data.end() >= start
                    && same_kind(&data, &b.data, by_stream)
            })
            .collect();
        if found.is_empty() {
            break;
        }
        for b in found {
            start = start.min(b.data.onset);
            end = end.max(b.data.end());
            merged.push(b.id);
        }
    }
    if merged.is_empty() {
        return Vec::new();
    }
    data.onset = start;
    data.duration = end - start;
    std::iter::once((id, Some(data)))
        .chain(merged.into_iter().map(|b| (b, None)))
        .collect()
}

/// What merging the shown annotations does to one group: `kept` grows to `data`, covering
/// the others, which are removed.
#[derive(Clone, Debug, PartialEq)]
pub struct Merged {
    pub kept: AnnotationId,
    pub data: AnnotationData,
    pub removed: Vec<AnnotationId>,
}

/// Merges, among the annotations `shown` accepts, those of a kind (see [`same_kind`], with
/// `by_stream`) that overlap or touch, directly or through others: the earliest of each
/// group (by onset, then id) keeps its id and origin and grows to cover the group, the
/// others are removed. Groups of one are left out, so the result is empty if there is
/// nothing to merge. Stim annotations never merge.
pub fn merge_shown(
    store: &AnnotationStore,
    by_stream: bool,
    shown: impl Fn(&AnnotationData) -> bool,
) -> Vec<Merged> {
    /// The group being gathered per kind, and where it ends so far.
    struct Run {
        merged: Merged,
        end: f64,
    }
    fn finish(run: Run, out: &mut Vec<Merged>) {
        let Run { mut merged, end } = run;
        if merged.removed.is_empty() {
            return;
        }
        // Only if it grew, so an annotation covering the others keeps its duration exactly.
        if end > merged.data.end() {
            merged.data.duration = end - merged.data.onset;
        }
        out.push(merged);
    }
    let mut runs: HashMap<(&str, Vec<ChannelId>, Option<usize>), Run> = HashMap::new();
    let mut out = Vec::new();
    // By onset, so a group is complete once an annotation of its kind starts after its end.
    for a in store.iter() {
        if a.data.origin == Origin::Stim || !shown(&a.data) {
            continue;
        }
        let mut channels = a.data.channels.clone();
        channels.sort();
        channels.dedup();
        let stream = if by_stream { a.data.stream } else { None };
        let key = (a.data.description.as_str(), channels, stream);
        // Closed intervals: one starting where the group ends touches it.
        match runs.get_mut(&key) {
            Some(run) if a.data.onset <= run.end => {
                run.end = run.end.max(a.data.end());
                run.merged.removed.push(a.id);
            }
            _ => {
                let run = Run {
                    merged: Merged {
                        kept: a.id,
                        data: a.data.clone(),
                        removed: Vec::new(),
                    },
                    end: a.data.end(),
                };
                if let Some(done) = runs.insert(key, run) {
                    finish(done, &mut out);
                }
            }
        }
    }
    for run in runs.into_values() {
        finish(run, &mut out);
    }
    out.sort_by_key(|m| m.kept);
    out
}
