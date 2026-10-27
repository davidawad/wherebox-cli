#!/bin/sh
# Cut a release: `just release <version>` (docs/releasing.md).
#
#   scripts/release.sh [--dry-run] <version>
#
# Checks for a clean master that matches origin/master, bumps Cargo.toml and
# Cargo.lock, prepends the CHANGELOG.md section (scripts/changelog.sh), commits "chore(release): v<version>", creates an
# annotated tag, pushes both and runs scripts/publish.sh.
# --dry-run runs the checks and shows the changes, then restores the files
# without committing, tagging or pushing.
set -eu

dry_run=
if [ "${1:-}" = --dry-run ]; then
  dry_run=1
  shift
fi
[ $# -eq 1 ] || {
  echo "usage: just release <version>   (e.g. just release 0.2.0)" >&2
  exit 2
}
version=${1#v}
tag=v$version
remote=${RELEASE_REMOTE:-origin}

die() {
  echo "release: $*" >&2
  exit 1
}

echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' ||
  die "'$version' is not a semver version like 0.2.0"
cd "$(git rev-parse --show-toplevel)"

branch=$(git symbolic-ref --short -q HEAD || true)
[ "$branch" = master ] || die "on '${branch:-detached HEAD}', not master"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"

git fetch --quiet --tags "$remote" master
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$remote/master")" ] ||
  die "master is not up to date with $remote/master (pull or push first)"
if git rev-parse -q --verify "refs/tags/$tag" > /dev/null; then die "tag $tag already exists"; fi

current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ "$current" != "$version" ] || die "Cargo.toml is already at $version"
highest=$(printf '%s\n%s\n' "$current" "$version" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1)
[ "$highest" = "$version" ] || die "$version is lower than the current version $current"

echo "Releasing $current -> $version"

# Cargo.toml: the first `version =` line is [package]. Cargo.lock: the
# wherebox-cli entry only (no dependency is updated).
sed "1,/^version = /s/^version = \".*\"$/version = \"$version\"/" Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml
awk -v v="$version" '
    /^name = "wherebox-cli"$/ { pkg = 1 }
    pkg && /^version = / { $0 = "version = \"" v "\""; pkg = 0 }
    { print }
' Cargo.lock > Cargo.lock.tmp
mv Cargo.lock.tmp Cargo.lock

scripts/changelog.sh "$version" CHANGELOG.md

git --no-pager diff --stat
git --no-pager diff CHANGELOG.md

if [ -n "$dry_run" ]; then
  git checkout -- Cargo.toml Cargo.lock CHANGELOG.md
  echo "Dry run: restored the files; nothing committed, tagged or pushed."
  exit 0
fi

git commit --quiet -m "chore(release): $tag" Cargo.toml Cargo.lock CHANGELOG.md
git tag -a "$tag" -m "wherebox-cli $tag"
git push --atomic "$remote" master "refs/tags/$tag"
echo "Pushed master and $tag to every push URL of $remote."

# No CI (no GitHub Actions or GitLab CI): the release is built and published
# from here (scripts/publish.sh).
scripts/publish.sh "$tag"
