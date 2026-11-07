use std::fmt;

use crate::error::{Result, StorageError};

/// Connection settings for an S3-compatible object store.
///
/// `Debug` is implemented manually and redacts credentials, so this type can be logged safely.
#[derive(Clone, PartialEq, Eq)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub path_style: bool,
}

impl StorageConfig {
    /// Env: SONORUS_S3_ENDPOINT, SONORUS_S3_BUCKET, SONORUS_S3_ACCESS_KEY_ID,
    /// SONORUS_S3_SECRET_ACCESS_KEY (all required → MissingEnv(name) if absent or empty);
    /// SONORUS_S3_REGION optional (if absent: region_from_endpoint(endpoint) else "auto");
    /// SONORUS_S3_PATH_STYLE optional: "1|true|yes" → true, "0|false|no" → false (case-insens.),
    /// other → InvalidConfig; default true. Calls validate() before returning.
    pub fn from_env() -> Result<Self> {
        let endpoint = required_env("SONORUS_S3_ENDPOINT")?;
        let bucket = required_env("SONORUS_S3_BUCKET")?;
        let access_key_id = required_env("SONORUS_S3_ACCESS_KEY_ID")?;
        let secret_access_key = required_env("SONORUS_S3_SECRET_ACCESS_KEY")?;

        let region = match optional_env("SONORUS_S3_REGION") {
            Some(region) => region,
            None => region_from_endpoint(&endpoint).unwrap_or_else(|| "auto".to_string()),
        };

        let path_style = match optional_env("SONORUS_S3_PATH_STYLE") {
            None => true,
            Some(value) => match value.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => true,
                "0" | "false" | "no" => false,
                _ => {
                    return Err(StorageError::InvalidConfig(format!(
                        "SONORUS_S3_PATH_STYLE has unrecognized value {value:?}"
                    )));
                }
            },
        };

        let config = Self {
            endpoint,
            region,
            bucket,
            access_key_id,
            secret_access_key,
            path_style,
        };
        config.validate()?;
        Ok(config)
    }

    /// Backblaze B2 preset: endpoint "https://s3.{region}.backblazeb2.com", path_style true.
    pub fn backblaze_b2(region: &str, bucket: &str, key_id: &str, secret: &str) -> Self {
        Self {
            endpoint: format!("https://s3.{region}.backblazeb2.com"),
            region: region.to_string(),
            bucket: bucket.to_string(),
            access_key_id: key_id.to_string(),
            secret_access_key: secret.to_string(),
            path_style: true,
        }
    }

    /// Cloudflare R2 preset: endpoint "https://{account_id}.r2.cloudflarestorage.com", region "auto", path_style true.
    pub fn cloudflare_r2(account_id: &str, bucket: &str, key_id: &str, secret: &str) -> Self {
        Self {
            endpoint: format!("https://{account_id}.r2.cloudflarestorage.com"),
            region: "auto".to_string(),
            bucket: bucket.to_string(),
            access_key_id: key_id.to_string(),
            secret_access_key: secret.to_string(),
            path_style: true,
        }
    }

    /// All string fields non-empty after trim → else InvalidConfig(field name);
    /// endpoint must start with "https://" or "http://" → else InvalidConfig. A trailing '/' is allowed.
    pub fn validate(&self) -> Result<()> {
        let fields = [
            ("endpoint", &self.endpoint),
            ("region", &self.region),
            ("bucket", &self.bucket),
            ("access_key_id", &self.access_key_id),
            ("secret_access_key", &self.secret_access_key),
        ];
        for (name, value) in fields {
            if value.trim().is_empty() {
                return Err(StorageError::InvalidConfig(format!(
                    "{name} must not be empty"
                )));
            }
        }
        let endpoint = self.endpoint.trim();
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err(StorageError::InvalidConfig(
                "endpoint must start with https:// or http://".to_string(),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for StorageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_hint = if self.access_key_id.chars().count() >= 8 {
            let prefix: String = self.access_key_id.chars().take(4).collect();
            format!("{prefix}…")
        } else {
            "…".to_string()
        };
        f.debug_struct("StorageConfig")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &key_hint)
            .field("secret_access_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// "https://s3.us-west-004.backblazeb2.com" → Some("us-west-004"); anything else → None.
pub fn region_from_endpoint(endpoint: &str) -> Option<String> {
    let s = endpoint.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_suffix('/').unwrap_or(s);
    let region = s.strip_prefix("s3.")?.strip_suffix(".backblazeb2.com")?;
    if region.is_empty() || region.contains(['.', '/']) {
        None
    } else {
        Some(region.to_string())
    }
}

/// Reads a required variable; absent, non-unicode, or blank-after-trim → MissingEnv(name).
fn required_env(name: &'static str) -> Result<String> {
    optional_env(name).ok_or(StorageError::MissingEnv(name))
}

/// Reads an optional variable; absent or blank-after-trim → None.
fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "wJalrXUtnFEMI-SECRET-VALUE-9z";
    const KEY_ID: &str = "AKIAEXAMPLEKEYID1234";

    #[test]
    fn debug_never_leaks_secret_or_full_key() {
        let config = StorageConfig::backblaze_b2("us-west-004", "music", KEY_ID, SECRET);
        let out = format!("{config:?}");
        assert!(!out.contains(SECRET));
        assert!(!out.contains("SECRET-VALUE"));
        assert!(!out.contains(KEY_ID));
        assert!(!out.contains("EXAMPLEKEYID"));
        assert!(out.contains("AKIA…"));
        assert!(out.contains("<redacted>"));
    }

    #[test]
    fn debug_short_key_is_fully_hidden() {
        let config = StorageConfig::backblaze_b2("us-west-004", "music", "abc", SECRET);
        let out = format!("{config:?}");
        assert!(!out.contains("abc"));
        assert!(!out.contains(SECRET));
        assert!(out.contains("\"…\""));
    }

    #[test]
    fn region_from_endpoint_cases() {
        assert_eq!(
            region_from_endpoint("https://s3.us-west-004.backblazeb2.com"),
            Some("us-west-004".to_string())
        );
        assert_eq!(
            region_from_endpoint("https://s3.eu-central-003.backblazeb2.com/"),
            Some("eu-central-003".to_string())
        );
        assert_eq!(
            region_from_endpoint("https://abc.r2.cloudflarestorage.com"),
            None
        );
        assert_eq!(region_from_endpoint("https://s3..backblazeb2.com"), None);
        assert_eq!(region_from_endpoint("https://s3.backblazeb2.com"), None);
    }

    #[test]
    fn backblaze_preset() {
        let c = StorageConfig::backblaze_b2("us-west-004", "music", KEY_ID, SECRET);
        assert_eq!(c.endpoint, "https://s3.us-west-004.backblazeb2.com");
        assert_eq!(c.region, "us-west-004");
        assert_eq!(c.bucket, "music");
        assert_eq!(c.access_key_id, KEY_ID);
        assert_eq!(c.secret_access_key, SECRET);
        assert!(c.path_style);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn cloudflare_preset() {
        let c = StorageConfig::cloudflare_r2("acct123", "music", KEY_ID, SECRET);
        assert_eq!(c.endpoint, "https://acct123.r2.cloudflarestorage.com");
        assert_eq!(c.region, "auto");
        assert_eq!(c.bucket, "music");
        assert!(c.path_style);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_bucket() {
        let c = StorageConfig::backblaze_b2("us-west-004", "  ", KEY_ID, SECRET);
        assert!(matches!(c.validate(), Err(StorageError::InvalidConfig(_))));
    }

    #[test]
    fn validate_rejects_endpoint_without_scheme() {
        let mut c = StorageConfig::backblaze_b2("us-west-004", "music", KEY_ID, SECRET);
        c.endpoint = "s3.us-west-004.backblazeb2.com".to_string();
        assert!(matches!(c.validate(), Err(StorageError::InvalidConfig(_))));
    }
}
