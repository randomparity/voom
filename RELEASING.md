# Releasing VOOM

This project uses SemVer for release versions and an explicit product version
for development builds.

## Version Policy

- Release versions are plain SemVer, such as `0.1.0`.
- Release tags use `vMAJOR.MINOR.PATCH`, such as `v0.1.0`.
- The release tag version must match `crates/voom-cli/Cargo.toml`.
- Development builds include the package version, a `-dev` suffix, and a commit
  identifier: `0.1.0-dev+g<short-sha>`.
- If Git metadata is unavailable, development builds use
  `0.1.0-dev+unknown`.
- Native plugin crate versions remain Cargo package versions. The product
  version policy applies to the `voom` CLI version shown by `voom --version`.

## 0.1.0 Release Process

1. Start from a clean working tree on `main`.
2. Confirm CI is green for the commit that will be released.
3. Confirm `crates/voom-cli/Cargo.toml` has the intended release version:

   ```sh
   cargo metadata --no-deps --format-version=1 \
     | jq -r '.packages[] | select(.name == "voom-cli") | .version'
   ```

4. Run the local release checks:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   cargo build --release --bin voom
   ./target/release/voom --version
   ```

   Before tagging, a non-release build should print a development version such
   as `voom 0.1.0-dev+g2b683f107a2e`.

5. Create and push the release tag:

   ```sh
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

6. Wait for the GitHub Actions `Release` workflow to finish. It validates that
   the tag matches the CLI package version, builds the release binary, and
   publishes a GitHub release.

7. Download the release artifact and smoke-test it:

   ```sh
   ./voom --version
   ```

   The expected output for the `v0.1.0` release is `voom 0.1.0`.

## Failed Release

If the release workflow fails before publishing an artifact, delete the local
and remote tag, fix the issue, and create the tag again from the corrected
commit:

```sh
git tag -d v0.1.0
git push origin :refs/tags/v0.1.0
```
