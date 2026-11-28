//! Trace drawing with wgpu.
//!
//! [`Frame::new`] places the tiles of a [`Query`](kymo_core::tile::Query) on screen on the
//! CPU; [`TraceRenderer`] uploads tiles once and draws a frame inside a render pass, such
//! as egui's paint callback. Each pixel column of each channel lights the rows between the
//! lowest and highest value that touches it, so peaks never disappear at any zoom.

mod frame;
mod gpu;

pub use frame::{
    ChannelDraw, Coefficients, EDGE_TOLERANCE, Frame, Hatch, Native, Part, Row, Scene, TileDraw,
    faded,
};
pub use gpu::{DEFAULT_GPU_BUDGET, RenderError, TraceRenderer, check_support};
