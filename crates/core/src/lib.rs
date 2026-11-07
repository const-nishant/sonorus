//! `sonorus-core`: the UniFFI surface (`libsonorus_core.so`) that the Android shell talks to.
//!
//! One [`core::SonorusCore`] object owns the tokio runtime, the library database, the bucket
//! client and the player. Every exported method is blocking and must be called off the
//! Android main thread (e.g. `Dispatchers.IO`). Events arrive on the "sonorus-events" thread
//! through [`events::EventListener`].

uniffi::setup_scaffolding!();

pub mod core;
pub mod error;
pub mod events;
pub mod router;
pub mod types;

pub use crate::core::SonorusCore;
pub use error::CoreError;
pub use events::EventListener;

/// Routes `log` output to logcat (tag "sonorus") on Android; a no-op elsewhere. Safe to call
/// more than once.
#[uniffi::export]
pub fn init_logging() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("sonorus"),
    );
}

/// Crate version, for the about screen and bug reports.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
