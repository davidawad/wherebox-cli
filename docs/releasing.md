# Release process

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
   it is installed, `cargo binstall git-cliff`; otherwise an awk fallback with
   the same groups);
4. commits `chore(release): v<version>`, creates the annotated tag
   `v<version>` and pushes both (`git push --atomic`).

Everything after the push happens in GitHub Actions:

| workflow | trigger | does |
|----------|---------|------|
| `release.yml` | tag `v*` | fails unless the tag is `v<Cargo.toml version>`; builds each target natively (toolchains per [platforms.md](platforms.md)); packages `wherebox-<version>-<target>.tar.gz` (`.zip` on Windows) with the binary, `LICENSE`, `README.md`, man pages and shell completions (`scripts/package.sh`); writes `SHA256SUMS`; creates the GitHub release with notes from git-cliff (commits since the previous tag) |
| `release.yml`, `homebrew` job | after the release | with the `HOMEBREW_TAP_TOKEN` secret: writes `Formula/wherebox.rb` (`scripts/homebrew-formula.sh`) to `davidawad/homebrew-tap`, or `davidawad/tap` if that is the one that exists. No secret or no tap: skipped with a notice |
| `mirror.yml` | push to `master`, any tag | with the `GITLAB_MIRROR_SSH_KEY` secret: pushes `master` and tags to `git@gitlab.com:davidawad/wherebox-cli.git`. No secret: skipped with a notice |
| `ci-full.yml` | tag `v*` | fmt, clippy and tests on all five platforms |

Write commit subjects as conventional commits (`feat:`, `fix(scope):`,
`docs:`, `feat!:` for breaking changes, ...): they become the changelog and
the release notes. `chore(release)` commits and non-conventional subjects are
left out.

## Secrets

* `HOMEBREW_TAP_TOKEN`: a fine-grained token with *Contents: read and write*
  on the tap repository.
* `GITLAB_MIRROR_SSH_KEY`: the private half of a GitLab deploy key with write
  access on the mirror project (allow it to push to the protected `master`
  branch, or unprotect it on the mirror).

## When something fails

* **Tag does not match `Cargo.toml`**: delete the tag (`git push origin
  :refs/tags/vX.Y.Z && git tag -d vX.Y.Z`), fix the version and release again.
* **A build or the upload failed**: fix the cause on `master` if needed, then
  re-run the failed jobs, or rebuild an existing tag with *Actions > Release >
  Run workflow* (`gh workflow run release.yml -f tag=vX.Y.Z`). Re-running
  replaces the assets and notes of the existing release.
