#!/usr/bin/env sh
# Regenerate every README sample from scratch with a fresh release build:
#
#   docs/readme-session.txt      the copyable session (plain text)
#   README.md                    the block between the readme-session markers
#
# tests/readme.rs fails when the committed session drifts from the binary.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

cargo build --release --quiet
bin_dir=$(cargo metadata --format-version 1 --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release
export PATH="$bin_dir:$PATH"

WHEREBOX_COLOR=never scripts/readme-session.sh full >docs/readme-session.txt

# Splice the session into README.md between its markers.
splice() {
    tmp=$(mktemp)
    awk -v session="$2" -v name="$1" '
        $0 == "<!-- " name ":begin -->" {
            print; print "```console"
            while ((getline line < session) > 0) print line
            print "```"; skip = 1; next
        }
        $0 == "<!-- " name ":end -->" { skip = 0 }
        !skip
    ' README.md >"$tmp"
    cat "$tmp" >README.md
    rm -f "$tmp"
}
splice readme-session docs/readme-session.txt

echo "regenerated docs/readme-session.txt and README.md"
