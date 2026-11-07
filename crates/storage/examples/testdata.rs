//! Uploads or removes test audio under a `sonorus-test/` key prefix of the configured bucket.
//!
//! cargo run -p sonorus-storage --example testdata -- upload <local_dir> [prefix]
//! cargo run -p sonorus-storage --example testdata -- clean [prefix]
//!
//! The prefix defaults to `sonorus-test/` and must start with it, so `clean` can never touch
//! anything else in the bucket. Keys are the files' paths relative to `local_dir`.

use std::path::{Path, PathBuf};

use sonorus_storage::{Storage, StorageConfig, content_type_for_key, normalize_prefix};

const ROOT: &str = "sonorus-test/";

fn files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = args
        .split_first()
        .ok_or("usage: testdata upload <dir> [prefix] | clean [prefix]")?;
    let prefix_arg = match cmd.as_str() {
        "upload" => rest.get(1),
        _ => rest.first(),
    };
    let prefix = normalize_prefix(prefix_arg.map_or(ROOT, String::as_str));
    if !prefix.starts_with(ROOT) {
        return Err(format!("prefix must start with {ROOT}").into());
    }
    let storage = Storage::new(&StorageConfig::from_env()?)?;

    match cmd.as_str() {
        "upload" => {
            let dir = PathBuf::from(rest.first().ok_or("missing <local_dir>")?);
            let mut paths = Vec::new();
            files(&dir, &mut paths)?;
            paths.sort();
            for path in paths {
                let rel = path
                    .strip_prefix(&dir)?
                    .to_string_lossy()
                    .replace('\\', "/");
                let key = format!("{prefix}{rel}");
                storage
                    .upload_file(&path, &key, Some(content_type_for_key(&key)), |_| {})
                    .await?;
                println!("uploaded {key}");
            }
        }
        "clean" => {
            let objects = storage.list(&prefix).await?;
            for object in &objects {
                storage.delete(&object.key).await?;
                println!("deleted {}", object.key);
            }
            println!("{} objects removed under {prefix}", objects.len());
        }
        other => return Err(format!("unknown command {other}").into()),
    }
    Ok(())
}
