## Summary

<!-- Briefly explain what this pull request changes and why. -->

## Related issue

<!-- Link an issue when applicable, for example: Closes #123. -->

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactor
- [ ] Documentation
- [ ] Tests
- [ ] CI / tooling
- [ ] Other

## Testing

<!-- Describe how you verified the change. Include relevant commands, test cases, or manual steps. -->

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --release --locked
```

## Checklist

- [ ] I kept this pull request focused on one problem or feature.
- [ ] I reviewed my own changes.
- [ ] I added or updated tests when behavior changed.
- [ ] I updated documentation when API, configuration, security, or deployment behavior changed.
- [ ] I did not include secrets, credentials, API keys, or private data.
- [ ] The formatting, Clippy, tests, and release build checks pass locally.
- [ ] I considered backward compatibility and migration impact where relevant.

## Additional notes

<!-- Add anything reviewers should know that does not fit above. -->
