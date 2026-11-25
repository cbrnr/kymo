//! Integration tests of kymo-core, one module per area.
//!
//! One binary for all of them, so they link once and run in parallel.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;
mod kinds;

mod annotation_file;
mod annotations;
mod brainvision;
mod bvrf;
mod cache;
mod channel;
mod channel_file;
mod decode;
mod edf_header;
mod edf_plus;
mod edf_reader;
mod eeglab;
mod fiff;
mod gdf;
mod grid;
mod irregular;
mod open;
mod prefetch;
mod pyramid;
mod query;
mod raw_tiles;
mod real_data;
mod recording;
mod service;
mod sidecar;
mod spread;
mod stim;
mod tile_cache;
mod tiles;
mod window_mean;
mod xdf;
