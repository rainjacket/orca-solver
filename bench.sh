#!/usr/bin/env bash
# Benchmark for Orca crossword solver.
# Usage: ./bench.sh [--parallel N]
#
# Runs bench_15x15.grid exhaustively with --disallow-shared-substring 0.
# Default: sequential (1 thread). Use --parallel N for N threads.
#
# Expects dictionaries/spreadthewordlist_caps.dict (~465K words).
# Download from: https://www.spreadthewordlist.com/

set -euo pipefail

source "$HOME/.cargo/env" 2>/dev/null || true

ORCA="${ORCA:-./target/release/orca}"
DICT="${DICT:-dictionaries/spreadthewordlist_caps.dict}"
PARALLEL=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --parallel)
            PARALLEL="$2"
            shift 2
            ;;
        *)
            echo "Usage: $0 [--parallel N]"
            exit 1
            ;;
    esac
done

if [ ! -f "$DICT" ]; then
    echo "Error: Dictionary not found at $DICT"
    echo ""
    echo "The benchmark expects Spread the Wordlist (~465K words):"
    echo "  https://www.spreadthewordlist.com/"
    echo ""
    echo "After downloading, place it in the dictionaries/ directory:"
    echo "  mv ~/Downloads/spreadthewordlist_caps.dict dictionaries/"
    echo ""
    echo "Or use a custom dictionary: DICT=/path/to/your.dict ./bench.sh"
    exit 1
fi

if [ ! -f "$ORCA" ]; then
    echo "Building release binary..."
    RUSTFLAGS="-C target-cpu=native" cargo build --release
fi

PARALLEL_ARGS=""
MODE="sequential"
if [ -n "$PARALLEL" ]; then
    PARALLEL_ARGS="-j $PARALLEL"
    MODE="parallel ($PARALLEL threads)"
fi

echo "Mode: $MODE"
echo "Binary: $ORCA"
echo "Dict: $DICT"
echo

echo "=== 15x15 (bench_15x15) ==="
# shellcheck disable=SC2086
time "$ORCA" fill grids/bench_15x15.grid "$DICT" \
    --disallow-shared-substring 0 -n 0 --progress-interval 100000 \
    $PARALLEL_ARGS 2>&1 \
    | grep -E '(^Final stats:|^Search exhausted|^Stopped after|\[partition\]|\[parallel\])'
