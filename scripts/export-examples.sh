#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked -p typst-cli
compiler="${TYPST_SVG_BIN:-./target/debug/typst}"
mkdir -p examples/output/{01-text,02-imported-svg,03-pages}
for example in 01-text 02-imported-svg; do
  "$compiler" compile "examples/$example.typ" "examples/output/$example/hybrid.svg" --svg-text hybrid
  "$compiler" compile "examples/$example.typ" "examples/output/$example/outline.svg" --svg-text outline
done
"$compiler" compile examples/03-pages.typ 'examples/output/03-pages/page-{p}.svg' --svg-text hybrid
"$compiler" compile examples/03-pages.typ 'examples/output/03-pages/outline-{p}.svg' --svg-text outline
