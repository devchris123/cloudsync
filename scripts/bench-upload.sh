#!/usr/bin/env bash
set -euo pipefail

# Usage: ./scripts/bench-upload.sh [size_mb] [batch_size]
# Requires: cloudsync init already run in a sync directory

SIZE_MB="${1:-100}"
BATCH_SIZE="${2:-5}"
SYNC_DIR="${CLOUDSYNC_BENCH_DIR:-/tmp/cloudsync-bench}"
TEST_FILE="$SYNC_DIR/bench-${SIZE_MB}mb.bin"

echo "=== CloudSync upload benchmark ==="
echo "File size:   ${SIZE_MB}MB"
echo "Batch size:  ${BATCH_SIZE}"
echo "Sync dir:    $SYNC_DIR"
echo ""

# Setup sync dir if needed
mkdir -p "$SYNC_DIR/.cloudsync"
if [ ! -f "$SYNC_DIR/.cloudsync/config.toml" ]; then
    echo "Error: run 'cloudsync init' in $SYNC_DIR first"
    exit 1
fi

# Generate test file
echo "Generating ${SIZE_MB}MB test file..."
dd if=/dev/urandom of="$TEST_FILE" bs=1M count="$SIZE_MB" 2>/dev/null
echo ""

# Run push and time it
echo "Pushing..."
cd "$SYNC_DIR"
time CLOUDSYNC_BATCH_SIZE="$BATCH_SIZE" cargo run --release -p cloudsync-client -- push

# Cleanup
echo ""
echo "Cleaning up test file..."
rm -f "$TEST_FILE"
echo "Done. Delete the file from the server manually if needed."
