# Editable text in editable SVG exports

The regular SVG export converts text to glyph outlines so that the rendered
appearance does not depend on fonts installed on the viewing device. The
standalone SVG CLI export also supports an opt-in hybrid mode:

```sh
typst compile document.typ --format svg --svg-text hybrid
```

Hybrid mode writes ordinary left-to-right Latin text runs as SVG `<text>`
elements. Each Typst text run becomes its own positioned text object; the
exporter does not reconstruct flowing paragraphs. It keeps using the regular
glyph renderer for runs that need unsupported scripts, variable fonts, color
glyphs, non-solid text paints, or vertical positioning.

The mode currently covers static fonts and Latin characters, common accents,
combining marks, and punctuation. Because SVG text is shaped again by the
viewer, spacing and glyph appearance can differ from Typst even when the same
font is available. Fonts are referenced by family and style but are not
embedded.

The CLI produces one SVG per page, as with the regular SVG exporter. In hybrid
mode, imported SVG images are expanded into grouped SVG elements on the page.
Their IDs and local references are renamed per placement, and their `<text>`
elements remain text. PNG and JPEG images remain embedded as data URLs. If an SVG
cannot be integrated safely, the exporter warns with the page and resource and
embeds a 300 ppi PNG; compilation fails if that fallback also fails. PDF images
converted internally to SVG are not expanded in this stage.

The default SVG export remains outline-based; use `--svg-text outline`
to request it explicitly. Hybrid output can be edited and saved as SVG.
