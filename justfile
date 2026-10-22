# pisbx — development tasks. Run `just` to list them, `just check` before a commit.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Keep absolute build paths out of shipped binaries: without this, libstd's
# panic locations bake $HOME into the executable.
remap := "--remap-path-prefix=" + env_var_or_default("HOME", "/Users") + "=/build"

# List the recipes
default:
    @just --list --unsorted

# Build the release binary into target/release
build:
    RUSTFLAGS="{{remap}}" cargo build --release

# Build without touching the network (needs a warm cargo cache or a vendor dir)
build-offline:
    RUSTFLAGS="{{remap}}" cargo build --release --offline

# Install into ~/.cargo/bin
install:
    RUSTFLAGS="{{remap}}" cargo install --path . --locked

# Run the unit tests
test:
    cargo test --locked

# Run the tests without touching the network
test-offline:
    cargo test --locked --offline

# Format the sources
fmt:
    cargo fmt

# Fail if formatting is off
fmt-check:
    cargo fmt --check

# Lint, with warnings treated as errors
lint:
    cargo clippy --all-targets --locked -- -D warnings

# Everything CI should run
check: fmt-check lint test

# Write a starter policy to ~/.config/pisbx/config.toml
init *args:
    cargo run --quiet --release -- --init {{args}}

# Show the effective policy and where each entry came from
explain *args:
    cargo run --quiet --release -- --explain {{args}}

# Print the resolved srt settings (pass flags directly, no `--` needed)
print-config *args:
    cargo run --quiet --release -- --print-config {{args}}

# Verify the boundary against the real srt (run outside any sandbox)
selftest:
    cargo run --quiet --release -- --selftest

# Show what would be published
package-list:
    cargo package --list --allow-dirty

# Inspect the dependency tree
deps:
    cargo tree --locked

# Show which crates are outdated (needs network)
outdated:
    cargo outdated

# Remove build artifacts
clean:
    cargo clean
