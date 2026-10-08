// Modified for Jean Porcel’s Typst Editable SVG extension (2026). See DEVELOPMENT.md.
use ecow::EcoString;
use ttf_parser::GlyphId;
use typst_library::layout::Dir;
use typst_library::layout::{Abs, Ratio, Size, Transform};
use typst_library::text::color::{
    GlyphFrame, GlyphFrameItem, glyph_frame, should_outline,
};
use typst_library::text::{FontFlags, TextItem, is_default_ignorable};
use typst_library::visualize::{FillRule, Paint, RelativeTo};

use crate::path::SvgPathBuilder;
use crate::write::{SvgElem, SvgIdRef, SvgTransform, SvgWrite};
use crate::{DedupId, SVGRenderer, State, SvgTextMode};

/// Represents a glyph to be rendered.
#[derive(Clone)]
pub enum RenderedGlyph {
    /// A frame that contains an image glpyh.
    Frame(GlyphFrame),
    /// A path is a sequence of drawing commands.
    ///
    /// It is in the format of `M x y L x y C x1 y1 x2 y2 x y Z`.
    Path(EcoString),
}

impl SVGRenderer<'_> {
    /// Render a text item. The text is rendered as a group of glyphs. We will
    /// try to render the text as SVG first, then bitmap, then outline. If none
    /// of them works, we will skip the text.
    pub(super) fn render_text(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
    ) {
        if self.text_mode == SvgTextMode::Hybrid && can_render_live_text(text) {
            self.render_live_text(svg, state, text);
            return;
        }

        let svg = &mut svg.elem("g");

        // Flip the transform since fonts use a Y-Up coordinate system.
        let state = state.pre_concat(Transform::scale(Ratio::one(), -Ratio::one()));
        svg.attr("transform", SvgTransform(state.transform));

        let mut x = Abs::pt(0.0);
        let mut y = Abs::pt(0.0);
        for glyph in &text.glyphs {
            let id = GlyphId(glyph.id);
            let x_offset = x + glyph.x_offset.at(text.size);
            let y_offset = y + glyph.y_offset.at(text.size);

            self.render_glyph(svg, &state, text, id, x_offset, y_offset);

            x += glyph.x_advance.at(text.size);
            y += glyph.y_advance.at(text.size);
        }
    }

    /// Render a simple text run as an SVG text object.
    fn render_live_text(&mut self, svg: &mut SvgElem, state: &State, text: &TextItem) {
        let mut element = svg.elem("text");
        if !state.transform.is_identity() {
            element.attr("transform", SvgTransform(state.transform));
        }

        let font = text.font.font();
        let info = font.info();
        let variant = info.variant;
        element
            .attr("x", 0.0)
            .attr("y", 0.0)
            .attr("font-family", info.family.as_str())
            .attr("font-size", text.size.to_pt())
            .attr(
                "font-style",
                match variant.style {
                    typst_library::text::FontStyle::Normal => "normal",
                    typst_library::text::FontStyle::Italic => "italic",
                    typst_library::text::FontStyle::Oblique => "oblique",
                },
            )
            .attr("font-weight", variant.weight.to_number() as f64)
            .attr("xml:lang", text.lang.as_str())
            .attr("xml:space", "preserve");
        element.attr_with("font-stretch", |attr| {
            attr.push_num(variant.stretch.to_ratio().get() * 100.0);
            attr.push_str("%");
        });

        self.write_fill(
            &mut element,
            &text.fill,
            FillRule::default(),
            Ratio::one(),
            Transform::identity(),
        );
        if let Some(stroke) = &text.stroke {
            self.write_stroke(&mut element, stroke, Ratio::one(), Transform::identity());
        }

        element.text(&text.text);
    }

    fn render_glyph(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
        glyph_id: GlyphId,
        x_offset: Abs,
        y_offset: Abs,
    ) {
        if should_outline(&text.font, glyph_id) {
            // Pre-scale outlined glyphs, so strokes and fill patterns don't
            // need to consider text size glyph scaling.
            let scale = Ratio::new(text.size.to_pt() / text.font.units_per_em());
            let key = (&text.font, glyph_id, scale);
            let (id, path) = self.glyphs.insert_with_val(key, || {
                let mut builder = SvgPathBuilder::with_scale(scale);
                text.font.ttf().outline_glyph(glyph_id, &mut builder)?;
                Some(RenderedGlyph::Path(builder.finsish()))
            });

            if path.is_some() {
                self.render_path_glyph(
                    svg, state, text, glyph_id, x_offset, y_offset, id,
                );
            }
        } else {
            // Image glyphs apply a `scale` at use site, since colr, svg-, and
            // bitmap glyph images are usually quite large, and having one glyph
            // per text size is a bit of a waste.
            let key = (&text.font, glyph_id);
            let (id, frame) = self.glyphs.insert_with_val(key, || {
                let frame = glyph_frame(&text.font, glyph_id.0)?;
                Some(RenderedGlyph::Frame(frame))
            });

            if frame.is_some() {
                self.render_image_glyph(svg, x_offset, y_offset, text, id);
            }
        }
    }

    /// Write a reference to an image glyph that is stored in font units.
    fn render_image_glyph(
        &mut self,
        svg: &mut SvgElem,
        x_offset: Abs,
        y_offset: Abs,
        text: &TextItem,
        id: DedupId,
    ) {
        let scale = Ratio::new(text.size.to_pt() / text.font.units_per_em());
        // Flip the transform again, since images are drawn Y-Down.
        let ts = Transform::translate(x_offset, y_offset)
            .pre_concat(Transform::scale(scale, -scale));

        svg.elem("use")
            .attr("xlink:href", SvgIdRef(id))
            .attr("transform", SvgTransform(ts));
    }

    /// Render a pre-scaled path glyph defined by an outline.
    #[expect(clippy::too_many_arguments)]
    fn render_path_glyph(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
        glyph_id: GlyphId,
        x_offset: Abs,
        y_offset: Abs,
        id: DedupId,
    ) {
        // Apply the transform here, because the state transform is used to draw
        // strokes and fills with gradients and tilings.
        let state = state.pre_concat(Transform::translate(x_offset, y_offset));

        let Some(glyph_size) = text.font.ttf().glyph_bounding_box(glyph_id) else {
            // This shouldn't happen, because the glyph has been successfully
            // outlined to create the path.
            return;
        };

        let aspect_ratio = Size::new(
            Abs::pt(glyph_size.width() as f64),
            Abs::pt(glyph_size.height() as f64),
        )
        .aspect_ratio();

        let mut use_ = svg.elem("use");
        use_.attr("xlink:href", SvgIdRef(id))
            .attr("x", x_offset.to_pt())
            .attr("y", y_offset.to_pt());

        self.write_fill(
            &mut use_,
            &text.fill,
            FillRule::default(),
            aspect_ratio,
            self.text_paint_transform(&state, &text.fill),
        );
        if let Some(stroke) = &text.stroke {
            self.write_stroke(
                &mut use_,
                stroke,
                aspect_ratio,
                self.text_paint_transform(&state, &stroke.paint),
            );
        }
    }

    fn text_paint_transform(&self, state: &State, paint: &Paint) -> Transform {
        match paint {
            Paint::Solid(_) => Transform::identity(),
            Paint::Gradient(gradient) => match gradient.unwrap_relative(true) {
                RelativeTo::Self_ => Transform::identity(),
                RelativeTo::Parent => Transform::scale(
                    Ratio::new(state.size.x.to_pt()),
                    Ratio::new(state.size.y.to_pt()),
                )
                .post_concat(state.transform.invert().unwrap()),
            },
            Paint::Tiling(tiling) => match tiling.unwrap_relative(true) {
                RelativeTo::Self_ => Transform::identity(),
                RelativeTo::Parent => state.transform.invert().unwrap(),
            },
        }
    }

    /// Build the glyph definitions.
    pub(super) fn write_glyph_defs(&mut self, svg: &mut SvgElem) {
        if self.glyphs.iter().all(|(_, g)| g.is_none()) {
            return;
        }

        let mut defs = svg.elem("defs");
        let glyphs = std::mem::take(&mut self.glyphs);
        for (id, glyph) in glyphs.iter() {
            let Some(glyph) = glyph else { continue };

            let mut symbol = defs.elem("symbol");
            symbol.attr("id", id);
            symbol.attr("overflow", "visible");

            match glyph {
                RenderedGlyph::Frame(frame) => {
                    let state = State::new(frame.size()).pre_translate(frame.item.pos());
                    match &frame.item {
                        GlyphFrameItem::Tofu(_, shape) => {
                            self.render_shape(&mut symbol, &state, shape);
                        }
                        GlyphFrameItem::Image(_, image, size) => {
                            self.render_image(&mut symbol, &state, image, size);
                        }
                    }
                }
                RenderedGlyph::Path(path) => {
                    symbol.elem("path").attr("d", path);
                }
            }
        }

        // The glyphs have been taken above, there shouldn't be any new glyphs
        // produced from writing the glyph definitions.
        assert!(self.glyphs.is_empty());
    }
}

/// Whether a text run can be represented reliably as ordinary SVG text.
/// Whether a text item can be serialized as a simple editable text run.
///
/// This deliberately shares the compatibility gate used by the hybrid SVG
/// exporter so that other exporters can make the same live-text decision.
pub fn can_render_live_text(text: &TextItem) -> bool {
    if text.text.is_empty()
        || text.lang.dir() != Dir::LTR
        || text.font.font().info().flags.contains(FontFlags::VARIABLE)
        || !matches!(text.fill, Paint::Solid(_))
        || text
            .stroke
            .as_ref()
            .is_some_and(|stroke| !matches!(stroke.paint, Paint::Solid(_)))
        || text.glyphs.iter().any(|glyph| {
            glyph.y_advance.get() != 0.0 || !should_outline(&text.font, GlyphId(glyph.id))
        })
    {
        return false;
    }

    text.text.chars().all(is_supported_live_text_char)
}

/// The initial live-text export supports Latin scripts and common punctuation.
fn is_supported_live_text_char(ch: char) -> bool {
    if ch.is_control() || is_default_ignorable(ch) {
        return false;
    }

    matches!(
        ch as u32,
        0x0020..=0x007E
            | 0x00A0..=0x024F
            | 0x0300..=0x036F
            | 0x1E00..=0x1EFF
            | 0x2000..=0x206F
    )
}
