//! Android trust store for the S3 HTTP client.
//!
//! `aws-sdk-s3`'s default HTTPS client uses `rustls-native-certs`, which on Android finds no
//! root certificates (it only knows a Termux path). On Android we instead build an explicit
//! client whose trust store holds the system CA certificates from the platform cacerts dirs.

use std::path::Path;

const BEGIN_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";
const END_MARKER: &[u8] = b"-----END CERTIFICATE-----";

#[cfg(target_os = "android")]
const APEX_CACERTS_DIR: &str = "/apex/com.android.conscrypt/cacerts";
#[cfg(target_os = "android")]
const SYSTEM_CACERTS_DIR: &str = "/system/etc/security/cacerts";

/// Position of the first occurrence of `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Extracts every `-----BEGIN CERTIFICATE-----` ... `-----END CERTIFICATE-----` block from
/// `data`. Each block is returned inclusive of both markers, plus one trailing line break
/// (`\n` or `\r\n`) when present. Text outside the blocks is ignored, and a BEGIN without a
/// matching END is dropped.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn pem_blocks(data: &[u8]) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    let mut pos = 0;
    while let Some(begin) = find(data, BEGIN_MARKER, pos) {
        let Some(end_marker) = find(data, END_MARKER, begin) else {
            break;
        };
        let mut end = end_marker + END_MARKER.len();
        if data[end..].starts_with(b"\r\n") {
            end += 2;
        } else if data[end..].starts_with(b"\n") {
            end += 1;
        }
        blocks.push(data[begin..end].to_vec());
        pos = end;
    }
    blocks
}

/// Reads every regular file directly inside `dir` (non-recursive; unreadable entries are
/// skipped) and collects the PEM certificate blocks of each file.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn load_cert_dir(dir: &Path) -> Vec<Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut certs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(data) = std::fs::read(&path) {
            certs.extend(pem_blocks(&data));
        }
    }
    certs
}

/// Whether `pem` holds a certificate that rustls accepts as a trust anchor. The SDK parses
/// custom certificates with `.expect(..)` when it builds its connector (on the first request),
/// so anything it would reject must be filtered out here or the app would panic.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn is_valid_root(pem: &[u8]) -> bool {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;

    let mut certs = CertificateDer::pem_slice_iter(pem);
    let (Some(Ok(der)), None) = (certs.next(), certs.next()) else {
        return false;
    };
    rustls::RootCertStore::empty().add(der).is_ok()
}

/// HTTP client whose trust store holds Android's system CA certificates. `None` when no
/// certificates could be loaded or the client fails to build; the caller then keeps the SDK
/// default client.
#[cfg(target_os = "android")]
pub(crate) fn android_http_client() -> Option<aws_sdk_s3::config::SharedHttpClient> {
    use aws_smithy_http_client::Builder;
    use aws_smithy_http_client::tls::rustls_provider::CryptoMode;
    use aws_smithy_http_client::tls::{Provider, TlsContext, TrustStore};

    let mut certs = load_cert_dir(Path::new(APEX_CACERTS_DIR));
    if certs.is_empty() {
        certs = load_cert_dir(Path::new(SYSTEM_CACERTS_DIR));
    }
    let found = certs.len();
    certs.retain(|pem| is_valid_root(pem));
    if certs.len() < found {
        log::warn!(
            "skipped {} unparsable system CA certificates",
            found - certs.len()
        );
    }
    if certs.is_empty() {
        log::warn!("no Android system CA certificates found; using default HTTP client");
        return None;
    }

    let count = certs.len();
    let trust_store = certs.into_iter().fold(TrustStore::empty(), |store, pem| {
        store.with_pem_certificate(pem)
    });
    let context = match TlsContext::builder().with_trust_store(trust_store).build() {
        Ok(context) => context,
        Err(err) => {
            log::warn!("failed to build TLS context from system CA certificates: {err}");
            return None;
        }
    };
    let client = Builder::new()
        .tls_provider(Provider::Rustls(CryptoMode::AwsLc))
        .tls_context(context)
        .build_https();
    log::info!("loaded {count} system CA certificates");
    Some(client)
}

#[cfg(test)]
mod root_tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/test-ca.pem");

    #[test]
    fn real_certificate_in_android_format_is_valid() {
        // PEM block followed by `openssl x509 -text` output, like Android's cacerts files.
        let blocks = pem_blocks(FIXTURE);
        assert_eq!(blocks.len(), 1);
        assert!(is_valid_root(&blocks[0]));
    }

    #[test]
    fn corrupt_certificates_are_rejected() {
        assert!(!is_valid_root(b""));
        assert!(!is_valid_root(
            b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
        ));
        assert!(!is_valid_root(
            b"-----BEGIN CERTIFICATE-----\n!!not base64!!\n-----END CERTIFICATE-----\n"
        ));
        let mut two = pem_blocks(FIXTURE).remove(0);
        two.extend(pem_blocks(FIXTURE).remove(0));
        assert!(!is_valid_root(&two), "exactly one certificate per entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT_A: &str = "-----BEGIN CERTIFICATE-----\nMIIAAA==\n-----END CERTIFICATE-----\n";
    const CERT_B: &str = "-----BEGIN CERTIFICATE-----\nMIIBBB==\n-----END CERTIFICATE-----\n";

    #[test]
    fn pem_block_is_extracted_without_trailing_text() {
        let data = format!("{CERT_A}Certificate:\n    Data: Version 3\n    Subject: CN=x\n");
        let blocks = pem_blocks(data.as_bytes());
        assert_eq!(blocks, vec![CERT_A.as_bytes().to_vec()]);
    }

    #[test]
    fn two_concatenated_certs_yield_two_blocks() {
        let data = format!("{CERT_A}{CERT_B}");
        let blocks = pem_blocks(data.as_bytes());
        assert_eq!(
            blocks,
            vec![CERT_A.as_bytes().to_vec(), CERT_B.as_bytes().to_vec()]
        );
    }

    #[test]
    fn data_without_pem_yields_nothing() {
        assert!(pem_blocks(b"just some text\nno certificates here\n").is_empty());
        assert!(pem_blocks(b"").is_empty());
        let unterminated = b"-----BEGIN CERTIFICATE-----\nMIIAAA==\n";
        assert!(pem_blocks(unterminated).is_empty());
    }

    #[test]
    fn crlf_line_endings_are_preserved_in_block() {
        let crlf = "-----BEGIN CERTIFICATE-----\r\nMIIAAA==\r\n-----END CERTIFICATE-----\r\n";
        let data = format!("{crlf}trailer\r\n{crlf}");
        let blocks = pem_blocks(data.as_bytes());
        assert_eq!(
            blocks,
            vec![crlf.as_bytes().to_vec(), crlf.as_bytes().to_vec()]
        );
    }

    #[test]
    fn load_cert_dir_collects_pem_from_regular_files_only() {
        let dir =
            std::env::temp_dir().join(format!("sonorus-storage-tls-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("a.0"), CERT_A).unwrap();
        std::fs::write(dir.join("b.0"), CERT_B).unwrap();
        std::fs::write(dir.join("junk.txt"), "not a certificate").unwrap();
        std::fs::write(dir.join("subdir").join("nested.0"), CERT_A).unwrap();

        let blocks = load_cert_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(blocks.len(), 2);
        assert!(blocks.contains(&CERT_A.as_bytes().to_vec()));
        assert!(blocks.contains(&CERT_B.as_bytes().to_vec()));
    }

    #[test]
    fn load_cert_dir_missing_directory_is_empty() {
        let missing = std::env::temp_dir().join("sonorus-storage-tls-does-not-exist");
        assert!(load_cert_dir(&missing).is_empty());
    }
}
