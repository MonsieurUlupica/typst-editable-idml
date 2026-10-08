# Manual SVG editor acceptance

Record SVG editor version, OS and installed fonts. Generate the examples
with scripts/export-examples.sh.

1. Open 01-text/hybrid.svg, select and replace Latin text; inspect accents,
   bold, italic and color. Check unsupported runs remain visible as outlines.
2. Open 02-imported-svg/hybrid.svg. Inspect both graphic instances, clipping
   and arrow markers. Ungroup and edit the source graphic’s text.
3. Compare the two SVGs in 03-pages with their outline counterparts.
4. Move the original PNG source and confirm the exported image remains visible.
5. Save as SVG, close and reopen; inspect text, paths, images and clipping.

These are acceptance steps, not a claim that every SVG editor version was tested.
Automated structural checks do not establish SVG rendering fidelity.
