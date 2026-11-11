# Releasing Sonorus

## How versions are computed

The Android app takes its version from git tags. The newest tag matching `v[0-9]*` names the
release:

- `versionName` is the tag without the `v`: `v0.13.0` gives `0.13.0`, `v0.13.1-rc.1` gives
  `0.13.1-rc.1`.
- `versionCode` is `major * 1000000 + minor * 1000 + patch` from the `X.Y.Z` part. A pre-release
  suffix is ignored for the code. The build fails if minor or patch is 1000 or more.
- Without any tag (or without git), the version is `0.0.0-dev` and `versionCode` is `1`.
- A debug build whose HEAD is not exactly on the tag gets `-dev+<short sha>` appended to
  `versionName`, for example `0.13.0-dev+c1f194b`. Its `versionCode` does not change.
- Release builds always use the tag as is.

Use full clones in CI (`fetch-depth: 0`). A shallow checkout has no tags and falls back to
`0.0.0-dev`.

## Commit messages

Releases are cut by [release-please](https://github.com/googleapis/release-please) from
[Conventional Commits](https://www.conventionalcommits.org/) on `main`:

- `feat:` adds a minor-version bump and a "Features" changelog entry.
- `fix:` adds a patch bump and a "Bug Fixes" entry.
- `perf:` adds a patch bump and a "Performance" entry.
- `feat!:` or a `BREAKING CHANGE:` footer marks a breaking change.
- Other types (`docs:`, `refactor:`, `test:`, `chore:`, `ci:`) do not trigger a release on their own.

## One-time setup

1. Create the GitHub repository and add it as the remote:
   `git remote add origin git@github.com:<owner>/sonorus.git`
2. Push `main` and the version tag:
   `git push -u origin main` and `git push origin v0.13.0`
3. Generate the upload keystore. Keep the passwords in a password manager:

   ```sh
   keytool -genkeypair -v -keystore sonorus-upload.jks -alias sonorus -keyalg RSA -keysize 4096 -validity 10000
   ```

4. Store the keystore as a base64 secret:

   ```sh
   base64 < sonorus-upload.jks | tr -d '\n' | gh secret set ANDROID_KEYSTORE_B64
   ```

5. Add the other three repository secrets: `SONORUS_KEYSTORE_PASSWORD` (the keystore password),
   `SONORUS_KEY_ALIAS` (`sonorus`), and `SONORUS_KEY_PASSWORD` (the key password).
6. Optionally add `SONORUS_S3_ENDPOINT`, `SONORUS_S3_BUCKET`, `SONORUS_S3_ACCESS_KEY_ID` and
   `SONORUS_S3_SECRET_ACCESS_KEY`. They are used only by the release leak check
   (`scripts/check-apk-secrets.sh`), which fails the release if any of these values appears in the
   APK or AAB. Missing secrets skip the check.

Without the keystore secrets the release workflow fails, because release artifacts must be signed.

## How a release happens

1. Merge commits to `main` using Conventional Commit messages.
2. release-please keeps a release PR open. It updates `CHANGELOG.md`, `version.txt` and
   `.github/.release-please-manifest.json`.
3. Merge the release PR. release-please creates the tag `vX.Y.Z` and the GitHub release. The same
   workflow run then builds the signed APK and AAB, checks them, and uploads
   `sonorus-vX.Y.Z.apk`, `sonorus-vX.Y.Z.aab` and `SHA256SUMS` to the release.

## Test prerelease

To test a build from any commit without merging a release PR:

1. Open **Actions > Release > Run workflow** and choose the branch.
2. Enter a `prerelease_tag` such as `v0.13.1-rc.1`. The workflow tags the selected commit with it,
   creates a GitHub prerelease, and uploads the signed artifacts.

The tag must be new. Delete the prerelease and its tag afterwards if you do not want to keep it.

## Keystore safety

Back up `sonorus-upload.jks` and its passwords somewhere safe and offline. Android only accepts
updates signed with the same key. Losing the keystore means existing installs cannot be updated,
and users would have to uninstall and reinstall the app. Never commit the keystore: `*.jks` and
`*.keystore` are gitignored.
