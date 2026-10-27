
# Local quality gate.
test-gate:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Regenerate the README sample session and README block.
readme:
    scripts/readme-samples.sh

# Coverage report (slow, instrumented build); not part of test-gate.
coverage:
    cargo llvm-cov --html

# Cut a release: bump, changelog, commit, annotated tag, push (docs/releasing.md).
release version:
    scripts/release.sh {{version}}

# Show what `just release` would change, without committing or pushing.
release-dry-run version:
    scripts/release.sh --dry-run {{version}}
