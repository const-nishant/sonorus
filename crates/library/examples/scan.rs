//! Scans the configured bucket into `sonorus.db` and `covers/` in the current directory, then
//! prints a summary of the catalogue.
//!
//! Run with `cargo run -p sonorus-library --example scan`. Storage settings come from the
//! environment (or `.env`), as read by `StorageConfig::from_env`.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use sonorus_library::Library;
use sonorus_library::scan::{BucketSource, scan};
use sonorus_storage::{Storage, StorageConfig};
use tokio::runtime::Handle;

const CONCURRENCY: usize = 4;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MS_PER_HOUR: f64 = 3_600_000.0;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();

    let config = StorageConfig::from_env()?;
    let storage = Storage::new(&config)?;
    let library = Library::open(Path::new("sonorus.db"), Path::new("covers"))?;
    let source = Arc::new(BucketSource::new(storage, Handle::current()));

    let mut last_printed = 0;
    let report = scan(&library, source, &config.prefix, CONCURRENCY, |p| {
        if p.done > last_printed && (p.done % 10 == 0 || p.done == p.total) {
            last_printed = p.done;
            println!(
                "scanned {}/{} (+{} ~{} -{} !{})",
                p.done, p.total, p.added, p.updated, p.removed, p.failed
            );
        }
    })
    .await?;

    let stats = library.stats()?;
    println!("tracks:   {}", stats.tracks);
    println!("albums:   {}", stats.albums);
    println!("artists:  {}", stats.artists);
    println!("size:     {:.2} GiB", stats.total_bytes as f64 / GIB);
    println!(
        "duration: {:.2} h",
        stats.total_duration_ms as f64 / MS_PER_HOUR
    );

    if !report.failures.is_empty() {
        println!("failures ({}):", report.failures.len());
        for failure in &report.failures {
            println!("  {}: {}", failure.key, failure.error);
        }
    }
    Ok(())
}
