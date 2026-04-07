# herdr task runner

# Run unit tests
test:
    cargo test
    python3 -m unittest scripts.test_changelog scripts.test_vendor_libghostty_vt

# Run PR CI checks
ci:
    cargo fmt --check
    cargo test

# Check formatting + run unit tests + maintenance script tests
check: ci
    python3 -m unittest scripts.test_changelog scripts.test_vendor_libghostty_vt

# Run the full local test suite
test-all: check

# Build release binary
build:
    cargo build --release

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Finalize changelog, bump version, commit, tag, push, trigger release build (usage: just release 0.1.1)
release version:
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: commit your changes first"; \
        exit 1; \
    fi
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    python3 scripts/changelog.py prepare --version {{version}}
    sed -i.bak 's/^version = ".*"/version = "{{version}}"/' Cargo.toml && rm -f Cargo.toml.bak
    cargo test --quiet
    python3 -m unittest scripts.test_changelog
    git add CHANGELOG.md Cargo.toml Cargo.lock
    git diff --cached --quiet || git commit -m "release: v{{version}}"
    git tag -a v{{version}} -m "v{{version}}"
    git push --follow-tags
    @echo "v{{version}} released — GitHub Actions building binaries"

# Update website/latest.json from a published GitHub release (usage: just update-latest-json 0.1.1)
update-latest-json version:
    python3 scripts/changelog.py sync-latest-json --version {{version}} --output website/latest.json

# Verify GitHub release, local manifest, live manifest, and asset URLs all agree (usage: just verify-release-state 0.1.1)
verify-release-state version:
    python3 scripts/changelog.py verify-release-state --version {{version}} --output website/latest.json

# Print default config
default-config:
    cargo run --release -- --default-config
