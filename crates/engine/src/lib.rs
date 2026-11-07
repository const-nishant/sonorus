//! Sonorus playback engine: streaming source, decoding, ring buffer, output, queue.

pub mod decoder;
pub mod error;
pub mod source;

pub use error::{EngineError, Result};
