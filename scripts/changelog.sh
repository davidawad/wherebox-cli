#!/bin/sh
# Prepend the CHANGELOG.md section for a new version, from the conventional
# commits since the previous v* tag. Uses git-cliff (cliff.toml, as the release
# notes do) when it is installed, else an awk fallback with the same groups.
#
#   scripts/changelog.sh <version> [file]
set -eu

version=$1
file=${2:-CHANGELOG.md}

prev=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
range=${prev:+$prev..}HEAD

section=$(mktemp)
trap 'rm -f "$section"' EXIT

if command -v git-cliff >/dev/null; then
    git cliff --unreleased --tag "v$version" --strip all >"$section"
else
    printf '## %s - %s\n' "$version" "$(date +%Y-%m-%d)" >"$section"
    # type(scope)!: description  ->  - **scope:** Description (**breaking**)
    git log --reverse --no-merges --format=%s "$range" | awk '
        BEGIN {
            n = split("Features|Bug fixes|Performance|Refactoring|Documentation|Tests|Build and CI|Dependencies|Other", title, "|")
        }
        /^chore\(release\)/ { next }
        match($0, /^[a-z]+(\([^)]*\))?!?: /) {
            head = substr($0, 1, RLENGTH - 2); desc = substr($0, RLENGTH + 1)
            breaking = head ~ /!$/; sub(/!$/, "", head)
            type = head; scope = ""
            if (i = index(head, "(")) { type = substr(head, 1, i - 1); scope = substr(head, i + 1, length(head) - i - 1) }
            if (type == "feat") g = 1
            else if (type == "fix") g = 2
            else if (type == "perf") g = 3
            else if (type == "refactor") g = 4
            else if (type == "doc" || type == "docs") g = 5
            else if (type == "test") g = 6
            else if (type == "build" || type == "ci") g = 7
            else if (type == "chore" && scope == "deps") g = 8
            else if (type == "chore" || type == "style" || type == "revert") g = 9
            else next
            line = "- " (scope != "" ? "**" scope ":** " : "") toupper(substr(desc, 1, 1)) substr(desc, 2)
            out[g] = out[g] line (breaking ? " (**breaking**)" : "") "\n"
        }
        END { for (g = 1; g <= n; g++) if (g in out) printf "\n### %s\n\n%s", title[g], out[g] }
    ' >>"$section"
fi

if [ -f "$file" ]; then
    # Insert before the first "## " heading, after the file header.
    awk -v sect="$section" '
        !done && /^## / { while ((getline l < sect) > 0) print l; print ""; done = 1 }
        { print }
        END { if (!done) { print ""; while ((getline l < sect) > 0) print l } }
    ' "$file" >"$file.tmp"
    mv "$file.tmp" "$file"
else
    { printf '# Changelog\n\n'; cat "$section"; } >"$file"
fi
