# ntn — read-only Notion CLI

binary_name := "ntn"
install_dir := env_var("HOME") / ".local/bin"
version := `grep -m1 '^version' Cargo.toml | cut -d'"' -f2`

default: build

# Build release binary and copy to ~/.local/bin
build:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked --release
    mkdir -p "{{install_dir}}"
    pending=$(mktemp "{{install_dir}}/.ntn-install.XXXXXX")
    trap 'rm -f "$pending"' EXIT
    install -m 755 target/release/{{binary_name}} "$pending"
    "$pending" --version
    mv -f "$pending" "{{install_dir}}/{{binary_name}}"
    echo "Installed {{binary_name}} -> {{install_dir}}/{{binary_name}}"

# Build locally and install on an SSH host without a release (defaults to mac).
[positional-arguments]
sync host="mac": build
    #!/usr/bin/env bash
    set -euo pipefail
    sync_host="$1"
    if [[ ! "$sync_host" =~ ^[a-zA-Z0-9][a-zA-Z0-9._@-]*$ ]]; then
        echo "error: use an SSH host alias, hostname, or user@host" >&2
        exit 1
    fi
    remote=$(ssh -o BatchMode=yes "$sync_host" 'uname -sm')
    ssh -o BatchMode=yes "$sync_host" 'mkdir -p ~/.local/bin ~/.cache/ntn-src'
    if [ "$remote" = "$(uname -sm)" ]; then
        scp -q target/release/{{binary_name}} "$sync_host":.cache/ntn-src/ntn-sync-binary
        sync_binary='.cache/ntn-src/ntn-sync-binary'
    else
        echo "$sync_host is $remote; syncing source and building there"
        rsync -az --delete \
            --include='/Cargo.toml' --include='/Cargo.lock' \
            --include='/rust-toolchain.toml' --include='/src/***' \
            --exclude='*' -e 'ssh -o BatchMode=yes' ./ "$sync_host":.cache/ntn-src/
        ssh -o BatchMode=yes "$sync_host" 'export PATH="$HOME/.cargo/bin:$PATH"; cd ~/.cache/ntn-src && cargo build --locked --release'
        sync_binary='.cache/ntn-src/target/release/ntn'
    fi
    ssh -o BatchMode=yes "$sync_host" "bash -s -- $sync_binary" <<'SH'
    set -euo pipefail
    pending=$(mktemp "$HOME/.local/bin/.ntn-sync.XXXXXX")
    trap 'rm -f "$pending"' EXIT
    install -m 755 "$HOME/$1" "$pending"
    "$pending" --version
    mv -f "$pending" "$HOME/.local/bin/ntn"
    echo "Installed $HOME/.local/bin/ntn"
    SH

# Install the agent skill into ~/.claude/skills/ntn
install-skill:
    rm -rf ~/.claude/skills/ntn
    mkdir -p ~/.claude/skills/ntn
    cp -r skills/ntn/* ~/.claude/skills/ntn/
    @echo "Skill installed to ~/.claude/skills/ntn/"

# Run tests
test:
    cargo test --locked

# Format code
fmt:
    cargo fmt

# Check formatting (CI gate)
fmt-check:
    cargo fmt --check

# Lint with warnings as errors
lint:
    cargo clippy --locked --all-targets -- -D warnings

# Format check + lint + tests
check: fmt-check lint test

# Build and run, e.g. `just run conv list`
run *ARGS:
    cargo run -- {{ARGS}}

# Print the current version
version:
    @echo {{version}}

# Bump the version, verify, commit, tag, and push main; CI publishes binaries.
[positional-arguments]
release new_version:
    bash scripts/release.sh "$1"
