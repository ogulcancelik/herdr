# herdr task runner

# Run unit tests
test:
    cargo test

# Run integration tests (LLM-based, requires pi + tmux)
test-integration:
    ./tests/integration/run_all.sh

# Run all tests
test-all: test test-integration

# Build release binary
build:
    cargo build --release

# Kill any leftover test tmux sessions and clean results
clean-tests:
    @for sock in ${TMPDIR:-/tmp}/herdr-test-sockets/*/tmux.sock; do \
        [ -S "$$sock" ] && tmux -S "$$sock" kill-server 2>/dev/null || true; \
    done
    @rm -rf ${TMPDIR:-/tmp}/herdr-test-sockets 2>/dev/null || true
    @rm -f tests/integration/results/*.json tests/integration/results/*.txt 2>/dev/null || true
    @echo "cleaned"

# Bump version, commit, tag, push, trigger release build (usage: just release 0.1.1)
release version:
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: commit your changes first"; \
        exit 1; \
    fi
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    sed -i.bak 's/^version = ".*"/version = "{{version}}"/' Cargo.toml && rm -f Cargo.toml.bak
    cargo test --quiet
    git add Cargo.toml Cargo.lock
    git diff --cached --quiet || git commit -m "release: v{{version}}"
    git tag v{{version}}
    git push --follow-tags
    @echo "v{{version}} released — GitHub Actions building binaries"

# Print default config
default-config:
    cargo run --release -- --default-config
