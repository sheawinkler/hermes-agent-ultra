//! Work session pipeline: domain POI + resolution + domain work package upload.

mod domain;
mod metrics;
mod pipeline;
mod resolution;

pub use pipeline::{spawn_session_end_pipeline, touch_active_session};
