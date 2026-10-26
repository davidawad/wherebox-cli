
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
