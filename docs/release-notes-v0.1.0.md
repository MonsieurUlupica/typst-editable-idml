# Typst Editable SVG v0.1.0

Experimental hybrid SVG export for SVG editor by Jean Porcel, based on Typst.
Compatible text is editable, imported SVGs become grouped vectors and unsupported
text uses outlines. Unsafe SVG imports use warned embedded 300 ppi PNG fallback.

Build: cargo build --release --locked -p typst-cli
Usage: target/release/typst compile document.typ 'page-{0p}.svg' --svg-text hybrid

Fonts are not embedded. Each Typst run is its own object; save edited SVG files.
Development assistance: OpenAI Codex / GPT-6.1 Sol; provenance in DEVELOPMENT.md.
Apache-2.0, with upstream and third-party notices retained.
