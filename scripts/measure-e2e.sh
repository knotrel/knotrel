#!/usr/bin/env bash
# End-to-End Measurement Tooling for Knotrel Feature 0.2.6
# Measures snapshot import, catch-up throughput, time-to-ready, pure query latency distribution,
# independent ground-truth oracle agreement, sampled maximum process RSS (polled every 25ms; brief peaks between intervals may not be captured) alongside final sampled process RSS, and restart recovery duration.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

PROFILE="dev"
CARGO_PROFILE_FLAG=""
DATASET="small"
RUNS=1
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            CARGO_PROFILE_FLAG="--release"
            shift
            ;;
        --dev)
            PROFILE="dev"
            CARGO_PROFILE_FLAG=""
            shift
            ;;
        --runs)
            RUNS="$2"
            shift 2
            ;;
        small|medium|large)
            DATASET="$1"
            shift
            ;;
        *)
            EXTRA_ARGS+=("$1")
            shift
            ;;
    esac
done

EVIDENCE_DIR="target/measurements"
mkdir -p "$EVIDENCE_DIR"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
EVIDENCE_FILE="${EVIDENCE_DIR}/measurement_${DATASET}_${PROFILE}_${TIMESTAMP}.md"

echo "==> Compiling knotrel-server and measure_e2e (${PROFILE} profile)..."
cargo build ${CARGO_PROFILE_FLAG} --bin knotrel-server --example measure_e2e

echo "==> Running reproducible end-to-end measurements (dataset: ${DATASET}, runs: ${RUNS}, profile: ${PROFILE})..."
cargo run ${CARGO_PROFILE_FLAG} --example measure_e2e -- \
    --dataset "${DATASET}" \
    --runs "${RUNS}" \
    --spawn-server \
    --output-file "${EVIDENCE_FILE}" \
    ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}

echo "==> Retained measurement evidence saved to ${EVIDENCE_FILE}"
