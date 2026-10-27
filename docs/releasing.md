# Release process

There is no CI (no GitHub Actions, no GitLab CI). Quality and releases run
locally.

## Before pushing

```sh
just test-gate   # cargo fmt --check, clippy -D warnings, tests
```

## Releasing

A release is one command on an up-to-date `master`:

```sh
just release-dry-run 0.2.0   # optional: show the bump and changelog, change nothing
just release 0.2.0
```

`just release` (`scripts/release.sh`):

1. refuses unless you are on `master`, the tree is clean, `master` equals
   `origin/master` after a fetch, the tag `v<version>` does not exist yet and
   the version is higher than the one in `Cargo.toml`;
2. sets the version in `Cargo.toml` and the `wherebox-cli` entry of
   `Cargo.lock` (no dependency is updated);
3. prepends a `CHANGELOG.md` section built from the conventional commits since
   the previous tag (`scripts/changelog.sh`: git-cliff with `cliff.toml` when
   it is installed, otherwise an awk fallback with the same groups);
4. commits `chore(release): v<version>`, creates the annotated tag and pushes
   both with `git push --atomic origin`. `origin` has two push URLs (GitHub and
   the GitLab mirror), so this one push keeps both in sync;
5. runs `scripts/publish.sh v<version>`: builds `cargo build --release` for
   this host, packages `wherebox-<version>-<target>.tar.gz` with the binary,
   `LICENSE`, `README.md`, man pages and shell completions
   (`scripts/package.sh`), writes `SHA256SUMS`, and creates the GitHub release
   with this version's `CHANGELOG.md` section as notes.

More targets: on another machine, check out the tag and run
`scripts/publish.sh v<version>`. It adds that host's archive and rewrites
`SHA256SUMS` over every asset. `RELEASE_NO_BUILD=1` publishes without a binary.

Homebrew: after the archives are up, `scripts/homebrew-formula.sh <version>
SHA256SUMS davidawad/wherebox-cli > Formula/wherebox.rb` in a checkout of
`davidawad/homebrew-tap` writes the formula; commit and push it there.

Write commit subjects as conventional commits (`feat:`, `fix(scope):`,
`docs:`, `feat!:` for breaking changes, ...): they become the changelog and
the release notes.

## When something fails

* **Push rejected**: nothing was published. Fix it, delete the local tag
  (`git tag -d vX.Y.Z`), reset the release commit and run `just release` again.
* **Build or upload failed**: the tag is already pushed. Fix the cause and run
  `scripts/publish.sh vX.Y.Z` again; it replaces the assets of the existing
  release.
