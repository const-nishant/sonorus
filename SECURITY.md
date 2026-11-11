# Security policy

## Supported versions

Only the latest release receives security fixes. Update to the newest release before you report
an issue.

## Reporting a vulnerability

Report vulnerabilities privately through GitHub's private vulnerability reporting:
https://github.com/const-nishant/sonorus/security/advisories/new

Never report a vulnerability in a public issue, pull request or discussion.

We will respond within 7 days.

## Scope

- Bucket credentials (the access key ID and secret) are encrypted with an AES-256-GCM key that is
  held in the Android Keystore before they are written to app storage. The app sends them only to
  the S3 endpoint the user configures.
- Release builds are checked before publishing: the release workflow fails if any bucket secret
  kept in CI appears in the release APK or AAB.
- Release artifacts carry build provenance attestations. Verify one with
  `gh attestation verify <file> --repo const-nishant/sonorus`.
