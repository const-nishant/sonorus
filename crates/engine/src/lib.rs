//! Sonorus playback engine: streaming source, decoding, ring buffer, output, queue.

pub mod aaudio;
pub mod cache;
pub mod decoder;
pub mod error;
pub mod opener;
pub mod output;
pub mod player;
pub mod queue;
pub mod render;
pub mod source;

pub use error::{EngineError, Result};
