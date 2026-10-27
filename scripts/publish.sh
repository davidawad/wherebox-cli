#!/bin/sh
# Build and publish a tagged release from this machine (there is no CI).
#
#   scripts/publish.sh <tag>        e.g. scripts/publish.sh v0.2.0
#
# 1. Builds `cargo build --release --locked` for this host and packages it
#    with scripts/package.sh into dist/ (skipped with a notice when
#    RELEASE_NO_BUILD=1 or the build is refused on this host).
# 2. Writes dist/SHA256SUMS.
# 3. Creates the GitHub release for <tag> with the tag's CHANGELOG.md section
#    as notes, or uploads to it when it already exists (re-running replaces
#    the assets).
# More targets: run `scripts/publish.sh <tag>` again on another host; it adds
# that host's archive and rewrites SHA256SUMS from every asset.
set -eu

[ $# -eq 1 ] || {
  echo "usage: scripts/publish.sh <tag>" >&2
  exit 2
}
tag=$1
version=${tag#v}
repo=${RELEASE_REPO:-davidawad/wherebox-cli}
cd "$(git rev-parse --show-toplevel)"
git rev-parse -q --verify "refs/tags/$tag" > /dev/null || {
  echo "publish: no tag $tag" >&2
  exit 1
}

target=$(rustc -vV | sed -n 's/^host: //p')
dist=dist/$tag
mkdir -p "$dist"

if [ "${RELEASE_NO_BUILD:-0}" = 1 ]; then
  echo "publish: RELEASE_NO_BUILD=1, no binary for $target"
elif cargo build --release --locked --target "$target"; then
  scripts/package.sh "$version" "$target" "$dist"
else
  echo "publish: the build failed or was refused on this host; publishing without a $target binary" >&2
fi

# Notes: this version's CHANGELOG.md section.
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
awk -v v="$version" '
    /^## / { on = (index($0, "[" v "]") || index($0, " " v)) ; if (!on && done) exit; if (on) { done = 1; next } }
    on { print }
' CHANGELOG.md > "$notes"
[ -s "$notes" ] || echo "Release $tag." > "$notes"

if gh release view "$tag" -R "$repo" > /dev/null 2>&1; then
  gh release download "$tag" -R "$repo" -D "$dist" --skip-existing -p 'wherebox-*' || true
else
  gh release create "$tag" -R "$repo" --title "$tag" --notes-file "$notes" --verify-tag
fi

(cd "$dist" && find . -maxdepth 1 -name 'wherebox-*' -type f | sed 's|^\./||' | sort | xargs -r shasum -a 256 > SHA256SUMS)
set -- "$dist"/wherebox-* "$dist/SHA256SUMS"
[ -e "$1" ] || set -- "$dist/SHA256SUMS"
gh release upload "$tag" -R "$repo" --clobber "$@"
echo "Published $tag: https://github.com/$repo/releases/tag/$tag"
