#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo llvm-cov clean --workspace

if [[ "${1:-}" == "--html" ]]; then
    cargo llvm-cov --workspace --locked --all-features --html
    echo "Coverage HTML report generated at target/llvm-cov/html/index.html"
elif [[ "${1:-}" == "--summary" ]]; then
    cargo llvm-cov --workspace --locked --all-features
else
    cargo llvm-cov --workspace --locked --all-features --remap-path-prefix --lcov --output-path lcov.info
    echo "Generated lcov.info successfully."
fi
