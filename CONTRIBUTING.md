# Contributing to Sonorus

## Prerequisites

- Rust 1.98.1. `rust-toolchain.toml` pins it, so rustup installs it on the first `cargo` run.
  Android builds also need the `aarch64-linux-android` target, which the toolchain file lists.
- The Android SDK with NDK 28.2.13676358, platform `android-37.0` and build-tools `37.0.0`.
- JDK 17.

## Build

```sh
cd android && ./gradlew :app:assembleDebug
```

## Optional dev bucket defaults

Debug builds can prefill the bucket form from a `.env` file at the repository root. The file is
gitignored. Set these names (never commit values):

- `SONORUS_S3_ENDPOINT`
- `SONORUS_S3_REGION`
- `SONORUS_S3_BUCKET`
- `SONORUS_S3_ACCESS_KEY_ID`
- `SONORUS_S3_SECRET_ACCESS_KEY`
- `SONORUS_S3_PREFIX`

`android/app/build.gradle.kts` reads them. An environment variable with the same name takes
precedence over `.env`. Only debug builds get these values, and they end up in the debug APK, so
do not share a debug APK built with them. Release builds never include them.

## Checks

These are the same checks that CI runs. Run them before you open a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check advisories licenses bans sources
cd android && ./gradlew :app:assembleDebug :app:lint
```

## Commits and pull requests

Use [Conventional Commits](https://www.conventionalcommits.org/) for commit messages and pull
request titles, for example `feat(library): add folder filter` or `fix(lyrics): keep the current
line centred`. release-please reads them to write the changelog and pick the version.

## Releases

See [docs/RELEASING.md](docs/RELEASING.md).

## Security

Report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md).

## Licence

Contributions are licensed under GPL-3.0-or-later, the same licence as the project.
