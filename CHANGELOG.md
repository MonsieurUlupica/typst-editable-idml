# Changelog

## v0.1.0 — experimental release — 2026-10-08

- Add `SvgTextMode::Outline` and `Hybrid` while retaining the existing SVG API.
- Add `--svg-text outline|hybrid` for standalone SVG output.
- Emit supported horizontal Latin runs as editable `<text>` objects.
- Integrate imported SVG artwork with grouped content and collision-free IDs.
- Preserve raster data URLs; add warned 300 ppi PNG fallback for unsafe imports.
- Add regression coverage, original public examples and documentation.

Release tag: `svg-v0.1.0`. Base: Typst 0.15.1,
commit e58a63af09032a486b12241d08ebd04131483221.
