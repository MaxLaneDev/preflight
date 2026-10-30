#!/usr/bin/env bash
set -euo pipefail
cmake -S . -B build
ctest --test-dir build --output-junit results.xml --output-on-failure
