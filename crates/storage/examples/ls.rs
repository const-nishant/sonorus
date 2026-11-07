use sonorus_storage::{Storage, StorageConfig};

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let config = match StorageConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };
    println!("{config:?}");

    let prefix = std::env::args().nth(1).unwrap_or_default();
    let storage = match Storage::new(&config) {
        Ok(storage) => storage,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };
    let objects = match storage.list(&prefix).await {
        Ok(objects) => objects,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    let mut total_bytes: u64 = 0;
    for object in &objects {
        println!("{:>12} {}", object.size, object.key);
        total_bytes += object.size;
    }
    println!("{} objects, {} bytes", objects.len(), total_bytes);
}
