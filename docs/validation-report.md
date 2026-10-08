# Publication validation — 2026-10-08

Scope: isolated SVG fork prepared for Jean Porcel, based on Typst commit
e58a63af09032a486b12241d08ebd04131483221. macOS, Rust 1.99.0.

## Automated results

- `cargo test --offline --locked -p typst-svg`: 4 tests passed.
- `cargo test --offline --locked -p typst-cli --test smoke`: 27 tests passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- `bash -n scripts/export-examples.sh`: passed.
- Example generation: 8 SVG files, valid XML, unique IDs and resolved local references.
- 01-text hybrid: 7 text objects; outline: 0 text objects.
- 02-imported-svg hybrid: 5 text objects, including the original graphic text twice;
  imported SVG content is integrated, and the PNG remains a data URI.
- 03-pages: exactly 2 hybrid pages and 2 outline counterparts.
- No absolute machine paths or external assets were found in example SVG references.

## Manual scope

Jean Porcel previously reported successful inspection of editorial exports in SVG editor.
The app version for that report was not recorded. The new public example set was validated
structurally; it has not been opened, edited and resaved in SVG editor in this preparation.
The repeatable manual steps are in svg-validation.md.

A successful XML parse or automated test does not establish fidelity across all SVG editor
versions, fonts, filters and imported artwork.
