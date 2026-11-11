## Summary

<!-- What does this change do, and why? -->

## Type

- [ ] feat
- [ ] fix
- [ ] perf
- [ ] refactor
- [ ] docs
- [ ] ci

The PR title must be a [Conventional Commit](https://www.conventionalcommits.org/), for example
`feat(library): add folder filter`. release-please reads the title to write the changelog and pick
the next version.

## Testing

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cd android && ./gradlew :app:assembleDebug :app:lint`
- [ ] Tested on device (phone model, and USB DAC model if used):

## Checklist

- [ ] No credentials, bucket names or endpoints in code, logs or screenshots
