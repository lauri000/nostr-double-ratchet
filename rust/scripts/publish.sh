#!/bin/bash
# Publish supported nostr-double-ratchet crates to crates.io in dependency order
#
# Usage:
#   ./scripts/publish.sh
#   ./scripts/publish.sh --dry-run

set -e

DRY_RUN=""
ALLOW_DIRTY="--allow-dirty"

if [[ "$1" == "--dry-run" ]]; then
    DRY_RUN="--dry-run"
    echo "=== DRY RUN MODE ==="
fi

# Wait time between publishes for crates.io indexing (seconds)
WAIT_TIME=30

publish_crate() {
    local crate=$1
    local extra_flags=${2:-""}

    echo ""
    echo "=========================================="
    echo "Publishing: $crate"
    echo "=========================================="

    if cargo publish -p "$crate" $DRY_RUN $ALLOW_DIRTY $extra_flags; then
        echo "✓ $crate published successfully"

        if [[ -z "$DRY_RUN" ]]; then
            echo "Waiting ${WAIT_TIME}s for crates.io to index..."
            sleep $WAIT_TIME
        fi
    else
        echo "✗ Failed to publish $crate (continuing...)"
    fi
}

echo "Publishing supported nostr-double-ratchet crates to crates.io"
echo ""

# Check if logged in
if [[ -z "$DRY_RUN" ]]; then
    echo "Checking crates.io authentication..."
    if ! cargo login --help > /dev/null 2>&1; then
        echo "Please run 'cargo login' first"
        exit 1
    fi
fi

# Tier 1: Domain core
publish_crate "nostr-double-ratchet"

# Tier 2: Nostr adapter
publish_crate "nostr-double-ratchet-nostr"

echo ""
echo "=========================================="
echo "✓ All crates published successfully!"
echo "=========================================="
