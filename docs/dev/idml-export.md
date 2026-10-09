# Typst to IDML export

The `typst-idml` crate exports a finished `PagedDocument` as IDML. The CLI
accepts `--format idml` or infers the format from an `.idml` output extension.
Use `--idml-package` to create a handoff folder with the IDML document, external
linked artwork, and a report of required fonts. `--pages` selects physical
pages.

```sh
typst compile document.typ document-package --idml-package --pages 16
```

For a source file named `document.typ`, the folder contains:

```text
document-package/
├── document.idml
├── Links/
│   ├── Graphic_1.svg
│   └── Image_2.png
├── Document fonts/
│   └── permitted-font-files...
├── Fuentes.txt
└── LEEME.txt
```

## Composition model

The export keeps Typst's fixed page layout. It writes a separate InDesign story
and text frame for each compatible Typst text run; editing text does not
recompose neighboring content. Text frames auto-size in height and width from
their top-left reference point, and do not introduce line breaks while sizing.
This lets InDesign fit its own composition of the unchanged Typst text instead
of relying only on Typst's font metrics. Longer edits can grow a frame into
nearby artwork or text because neighboring content does not move. Text uses the
same compatibility predicate as the hybrid SVG exporter. Unsupported text
remains vector artwork.

The IDML document uses facing pages with left-to-right binding. Page 1 is a
standalone right-hand cover; following pages are paired in their source order
(2–3, 4–5, and so on). If an even-numbered final page has no partner, it remains
alone on the left as the back cover. When `--pages` selects only part of the
document, only adjacent source pages that form a normal pair are combined.
Page-item transforms include the spread offset, preserving each page's original
Typst composition. Each generated spread keeps page shuffling disabled so
InDesign preserves these explicit page groups instead of repaginating them.

Shapes and vector content are separate, cropped SVG files in the sibling
`Links` directory, referenced with relative `file:Links/...` URIs. Imported
SVG artwork is kept together as one placed SVG when it appears as one Typst
image object. Raster images outside clipped groups are converted to PNG and
written beside the document under `Links`. When clipping must be preserved,
the raster is embedded in a self-contained SVG resource. Keep the IDML document
and `Links` folder together when moving the package. Item transforms, clipping
groups, and z-order are preserved by exporting each vector item from its
isolated Typst frame.

The public APIs are `typst_idml::idml(&PagedDocument, &IdmlOptions)` for a
single IDML archive and `typst_idml::idml_package` for an IDML archive plus
external link files and a list of font families and styles. Set
`IdmlOptions.pages` to a `PageRanges` value to restrict output.

## Current limits

- Text frames are independent. InDesign may grow a frame to fit its story;
  neighboring objects do not move, so edited text can overlap nearby content.
  The original Typst composition remains anchored at the frame's top-left and
  baseline.
- Unsupported scripts and advanced glyph rendering are represented as vector
  shapes. Text inside a placed SVG remains part of that graphic and is not an
  InDesign story. For IDML, that SVG's text is converted to vector outlines so
  InDesign does not flag fonts used only inside a linked graphic as missing;
  the paths remain vector-editable, but the words are no longer text objects.
- Drop-shadow-only filters (`feDropShadow`) are removed from imported SVGs in
  IDML-linked artwork. InDesign may omit the entire filtered group, including
  its underlying shapes and text paths, so the exporter removes the shadow
  effect while retaining the group's vector content. This does not change the
  regular SVG export.
- Text styles use each font's OpenType typographic family and subfamily names
  when available, so InDesign can match faces whose internal style name differs
  from Typst's normalized weight (for example, `Geller` / `Headline Bold`).
  Variable fonts keep the style resolved from Typst. Fonts used by editable
  Typst text are copied to `Document fonts` only when OpenType permissions allow
  editable use. Restricted, preview-and-print-only, unsupported, or unavailable
  fonts are reported in `Fuentes.txt`. InDesign temporarily loads copied fonts
  from this folder when the IDML is opened.
- The IDML does not embed fonts. When a font cannot be included, install a
  licensed copy on the receiving computer; the package report names each face.
- The exporter emits IDML, not `.indd`. Open the package in InDesign and save it
  manually as an InDesign document.
- Structural package checks and CLI tests do not replace opening, editing, and
  resaving the pilot in InDesign. That final compatibility check is manual.
