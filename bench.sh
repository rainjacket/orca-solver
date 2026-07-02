#!/usr/bin/env bash
# Benchmark for the Orca crossword solver.
# Usage: ./bench.sh [--parallel N]
#
# Runs both 15x15 benchmark grids exhaustively:
#   bench_15x15.grid             (--disallow-shared-substring 0)
#   bench_15x15_with_tiers.grid  (--disallow-shared-substring 6)
#
# Bring your own wordlist. We recommend Spread the Wordlist (~300K entries),
# from https://www.spreadthewordlist.com/ — or point DICT at any list in
# WORD;SCORE format:
#   DICT=/path/to/your.dict ./bench.sh
#
# Each grid ships with a small companion supplement in dictionaries/ (the
# entries of a known fill). The supplement is appended to your wordlist at
# run time, so each grid is guaranteed fillable regardless of which list you
# use. Solution counts and timings still depend on your list.

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
    echo "Bring your own wordlist. We recommend Spread the Wordlist (~300K entries):"
    echo "  https://www.spreadthewordlist.com/"
    echo ""
    echo "After downloading, place it in the dictionaries/ directory:"
    echo "  mv ~/Downloads/spreadthewordlist_caps.dict dictionaries/"
    echo ""
    echo "Or use any dictionary in WORD;SCORE format: DICT=/path/to/your.dict ./bench.sh"
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

COMBINED="$(mktemp -t orca_bench_dict)"
trap 'rm -f "$COMBINED"' EXIT

run_bench() {
    local title="$1" grid="$2" supplement="$3"
    shift 3
    cat "$DICT" "$supplement" > "$COMBINED"
    echo "=== $title ==="
    echo "(dict: $DICT + $(basename "$supplement"))"
    # shellcheck disable=SC2086
    time "$ORCA" fill "$grid" "$COMBINED" "$@" -n 0 --progress-interval 100000 \
        $PARALLEL_ARGS 2>&1 \
        | grep -E '(^Final stats:|^Search exhausted|^Stopped after|\[partition\]|\[parallel\])'
    echo
}

echo "Mode: $MODE"
echo "Binary: $ORCA"
echo

run_bench "15x15 (bench_15x15)" \
    grids/bench_15x15.grid dictionaries/bench_15x15_supplement.dict \
    --disallow-shared-substring 0

run_bench "15x15 with scan-order tiers (bench_15x15_with_tiers)" \
    grids/bench_15x15_with_tiers.grid dictionaries/bench_15x15_with_tiers_supplement.dict \
    --disallow-shared-substring 6
