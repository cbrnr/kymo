//! Rows moving to their new places when the rows change (channels shown, hidden or
//! reordered): a row that goes collapses, its trace flattening and fading out, while the
//! others slide and resize into the room; a row that comes back opens the other way. Rows
//! slide likewise after a jump to a channel (see [`ViewState::jumps`]).

use kymo_core::ChannelId;

use crate::view::ViewState;

/// Seconds the rows take to reach their new places.
pub const SECONDS: f64 = 0.15;

/// Where a channel's row is drawn, in fractions of the trace area's height from its top,
/// and how opaque it is (0 for a hidden channel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub top: f64,
    pub height: f64,
    pub opacity: f64,
}

impl Slot {
    /// The middle of the row.
    pub fn center(&self) -> f64 {
        self.top + self.height / 2.0
    }

    fn lerp(self, to: Slot, t: f64) -> Slot {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Slot {
            top: mix(self.top, to.top),
            height: mix(self.height, to.height),
            opacity: mix(self.opacity, to.opacity),
        }
    }

    /// Whether any of it shows in the trace area.
    pub fn shows(&self) -> bool {
        self.opacity > 0.0 && self.height > 0.0 && self.top < 1.0 && self.top + self.height > 0.0
    }
}

/// The rows of a view: which channels, in what order, scrolled how far, how many on screen.
#[derive(Clone, Debug)]
struct Layout {
    rows: Vec<ChannelId>,
    order: Vec<ChannelId>,
    /// Per channel (by id): its place in `order`.
    place: Vec<usize>,
    top: f64,
    on_screen: f64,
    jumps: u64,
}

impl Layout {
    fn of(view: &ViewState) -> Layout {
        let order = view.order().to_vec();
        let mut place = vec![0; order.len()];
        for (i, c) in order.iter().enumerate() {
            place[c.0 as usize] = i;
        }
        Layout {
            rows: view.row_channels().to_vec(),
            order,
            place,
            top: view.top(),
            on_screen: view.rows() as f64,
            jumps: view.jumps(),
        }
    }

    /// Where `channel`'s row is: for a hidden channel, an empty row where it would be,
    /// between the rows of its neighbors.
    fn slot(&self, channel: ChannelId) -> Slot {
        let place = self.place[channel.0 as usize];
        let (row, shown) = match self
            .rows
            .binary_search_by_key(&place, |c| self.place[c.0 as usize])
        {
            Ok(row) => (row, true),
            Err(row) => (row, false),
        };
        let shown = f64::from(u8::from(shown));
        Slot {
            top: (row as f64 - self.top) / self.on_screen,
            height: shown / self.on_screen,
            opacity: shown,
        }
    }
}

/// Moves a view's rows from where they were drawn to where they are, after a change.
#[derive(Clone, Debug, Default)]
pub struct RowMotion {
    /// The view's rows as last followed.
    layout: Option<Layout>,
    /// Per channel (by id): where its row was when the rows changed; empty without a
    /// change in progress.
    from: Vec<Slot>,
    start: f64,
    seconds: f64,
    /// The time of the last [`follow`](Self::follow), at which rows are drawn.
    now: f64,
    /// Whether the rows set off at the last [`follow`](Self::follow).
    started: bool,
}

impl RowMotion {
    pub fn new() -> RowMotion {
        RowMotion::default()
    }

    /// Follows `view` at time `now` (seconds): when its rows changed since the last call,
    /// or it jumped to a channel, they set off from where they are drawn now, to arrive
    /// after `seconds` (at once for 0); a jump of more than a screen sets off a screen
    /// away, so its rows don't rush by. Other scrolling and zooming move them at once,
    /// wherever they are going. Returns whether they are still moving.
    pub fn follow(&mut self, view: &ViewState, now: f64, seconds: f64) -> bool {
        let then = std::mem::replace(&mut self.now, now);
        self.started = false;
        match &mut self.layout {
            Some(layout)
                if layout.rows == view.row_channels()
                    && layout.order == view.order()
                    && layout.jumps == view.jumps() =>
            {
                layout.top = view.top();
                layout.on_screen = view.rows() as f64;
            }
            Some(layout) => {
                let jump = layout.rows == view.row_channels() && layout.order == view.order();
                if jump && self.from.is_empty() {
                    let screen = view.rows() as f64;
                    layout.top = layout.top.clamp(view.top() - screen, view.top() + screen);
                }
                // From where the last frame drew them.
                self.now = then;
                let from = (0..view.order().len() as u32)
                    .map(|c| self.slot(ChannelId(c)))
                    .collect();
                self.now = now;
                self.from = from;
                self.start = now;
                self.seconds = seconds;
                self.started = true;
                self.layout = Some(Layout::of(view));
            }
            None => self.layout = Some(Layout::of(view)),
        }
        if self.progress().is_none() {
            self.from.clear();
        }
        !self.from.is_empty()
    }

    /// Whether the rows set off at the last [`follow`](Self::follow) (and are moving).
    pub fn started(&self) -> bool {
        self.started && self.progress().is_some()
    }

    /// How far the rows have come, eased (fast at first, slowing down as they arrive);
    /// `None` when they aren't moving.
    pub fn progress(&self) -> Option<f64> {
        if self.from.is_empty() || self.seconds.is_nan() || self.seconds <= 0.0 {
            return None;
        }
        let t = (self.now - self.start) / self.seconds;
        (t < 1.0).then(|| 1.0 - (1.0 - t.max(0.0)).powi(3))
    }

    /// Where `channel`'s row is drawn, as last followed; an empty slot (no height, no
    /// opacity) for a hidden channel once it is gone.
    pub fn slot(&self, channel: ChannelId) -> Slot {
        let Some(layout) = &self.layout else {
            return Slot {
                top: 0.0,
                height: 0.0,
                opacity: 0.0,
            };
        };
        let to = layout.slot(channel);
        match (self.progress(), self.from.get(channel.0 as usize)) {
            (Some(t), Some(&from)) => from.lerp(to, t),
            _ => to,
        }
    }

    /// The rows to draw, as last followed: the channels with any of their rows in the
    /// trace area, top to bottom. Without a change in progress, the view's rows on screen.
    pub fn rows(&self, view: &ViewState) -> Vec<(ChannelId, Slot)> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        let channels: &[ChannelId] = if self.progress().is_some() {
            &layout.order
        } else {
            view.visible_channels()
        };
        channels
            .iter()
            .map(|&c| (c, self.slot(c)))
            .filter(|(_, slot)| slot.shows())
            .collect()
    }
}
