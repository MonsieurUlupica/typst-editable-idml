//! Exporting Typst documents to InDesign Markup Language (IDML).
//!
//! The exporter keeps Typst's fixed page composition. Compatible horizontal
//! Latin text runs become independent InDesign stories and text frames; other
//! page items are packaged as independently placed SVG or raster resources.

use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use ecow::{EcoString, EcoVec};
use image::ImageFormat;
use ttf_parser::name_id;
use typst_layout::PagedDocument;
use typst_library::World;
use typst_library::diag::{SourceDiagnostic, SourceResult, Warned};
use typst_library::layout::{
    Abs, Frame, FrameItem, FrameKind, GroupItem, PageRanges, Point, Ratio, Rect, Size,
    Transform,
};
use typst_library::text::{Font, FontFlags, FontStyle, FontVariant, TextItem};
use typst_library::visualize::{Color, Image, ImageKind, Paint, Shape};
use typst_syntax::Span;
use typst_utils::Numeric;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

mod svg_fonts;

/// Options for exporting an IDML document.
#[derive(Debug, Default, Clone)]
pub struct IdmlOptions {
    /// A one-based set of pages to include. `None` exports every page.
    pub pages: Option<PageRanges>,
}

/// A complete IDML handoff: the IDML document, external linked assets, and
/// fonts that can be loaded from a sibling `Document fonts` directory.
#[derive(Debug, Clone)]
pub struct IdmlPackage {
    /// The IDML archive. Linked assets are stored beside it, not inside it.
    pub document: Vec<u8>,
    /// Resource files to write under the package's `Links` directory.
    pub links: Vec<(PathBuf, Vec<u8>)>,
    /// Fonts used by live text and linked SVGs, with permitted font data.
    pub fonts: Vec<IdmlFont>,
}

/// A font file used by editable text or linked artwork in the export.
#[derive(Debug, Clone)]
pub struct IdmlFont {
    /// Family and face names represented by this file.
    pub faces: Vec<String>,
    /// Safe filename to write under `Document fonts`, when permitted.
    pub filename: Option<PathBuf>,
    /// Original font file bytes, when permitted.
    pub data: Option<Vec<u8>>,
    /// Why the font file was not included.
    pub omitted_reason: Option<String>,
    /// Linked SVG resources that use this face.
    pub sources: Vec<PathBuf>,
}

/// Export a paged Typst document to an IDML archive.
pub fn idml(
    document: &PagedDocument,
    options: &IdmlOptions,
) -> Warned<SourceResult<Vec<u8>>> {
    let mut warnings = EcoVec::new();
    let result = export(document, options, &mut warnings)
        .and_then(|(files, _)| package(files))
        .map_err(|message| {
            let mut errors = EcoVec::new();
            errors.push(SourceDiagnostic::error(Span::detached(), message));
            errors
        });
    Warned { output: result, warnings }
}

/// Export an IDML handoff with linked artwork and permitted font files stored
/// beside the IDML document.
pub fn idml_package(
    document: &PagedDocument,
    options: &IdmlOptions,
) -> Warned<SourceResult<IdmlPackage>> {
    idml_package_impl(document, options, None)
}

/// Export an IDML handoff and resolve fonts referenced by placed SVG artwork
/// using the same font catalog that compiled the Typst document.
pub fn idml_package_with_world<W: World>(
    document: &PagedDocument,
    options: &IdmlOptions,
    world: &W,
) -> Warned<SourceResult<IdmlPackage>> {
    idml_package_impl(document, options, Some(world))
}

trait SvgFontProvider {
    fn select_font(&self, family: &str, variant: FontVariant) -> Option<Font>;
}

impl<W: World> SvgFontProvider for W {
    fn select_font(&self, family: &str, variant: FontVariant) -> Option<Font> {
        let index = self.book().select(&family.to_lowercase(), variant)?;
        self.font(index)
    }
}

fn idml_package_impl(
    document: &PagedDocument,
    options: &IdmlOptions,
    font_provider: Option<&dyn SvgFontProvider>,
) -> Warned<SourceResult<IdmlPackage>> {
    let mut warnings = EcoVec::new();
    let result =
        export(document, options, &mut warnings).and_then(|(files, mut fonts)| {
            let (mut links, document_files): (Vec<_>, Vec<_>) =
                files.into_iter().partition(|(path, _)| path.starts_with("Links/"));
            for (name, contents) in &mut links {
                if !name.to_ascii_lowercase().ends_with(".svg") {
                    continue;
                }
                let (normalized, font_uses) = svg_fonts::normalize_and_collect(contents)
                    .map_err(|error| {
                        format!("could not inspect fonts in {name} ({error})")
                    })?;
                *contents = normalized;
                let source = Path::new(name);
                for usage in font_uses {
                    collect_svg_font(usage, source, font_provider, &mut fonts);
                }
            }
            let document = package(document_files)?;
            let links = links
                .into_iter()
                .map(|(path, bytes)| {
                    let name = Path::new(&path)
                        .file_name()
                        .expect("generated link paths always have a filename");
                    (PathBuf::from(name), bytes)
                })
                .collect();
            Ok(IdmlPackage {
                document,
                links,
                fonts: fonts.into_values().collect(),
            })
        });
    let output = result.map_err(|message| {
        let mut errors = EcoVec::new();
        errors.push(SourceDiagnostic::error(Span::detached(), message));
        errors
    });
    Warned { output, warnings }
}

fn collect_svg_font(
    usage: svg_fonts::SvgFontUse,
    source: &Path,
    provider: Option<&dyn SvgFontProvider>,
    fonts: &mut BTreeMap<String, IdmlFont>,
) {
    let face_name =
        font_style_name(usage.variant.style, usage.variant.weight.to_number());
    let Some(provider) = provider else {
        add_missing_svg_font(
            &usage.family,
            &face_name,
            source,
            "no font catalog was supplied for SVG font resolution".to_owned(),
            fonts,
        );
        return;
    };

    let Some(font) = provider.select_font(&usage.family, usage.variant) else {
        add_missing_svg_font(
            &usage.family,
            &face_name,
            source,
            "the requested family and style were not found in Typst's font catalog"
                .to_owned(),
            fonts,
        );
        return;
    };
    if !font.info().family.eq_ignore_ascii_case(&usage.family)
        || font.info().variant.style != usage.variant.style
        || font.info().variant.weight != usage.variant.weight
        || font.info().variant.stretch != usage.variant.stretch
    {
        add_missing_svg_font(
            &usage.family,
            &face_name,
            source,
            "the font catalog has no exact face for the requested SVG style".to_owned(),
            fonts,
        );
        return;
    }

    let (family, style) = idml_font_names(&font, &face_name);
    add_font(&font, &family, &style, Some(source), fonts);
}

fn add_missing_svg_font(
    family: &str,
    style: &str,
    source: &Path,
    reason: String,
    fonts: &mut BTreeMap<String, IdmlFont>,
) {
    let face = format!("{family} — {style}");
    let key = format!("missing:{}", face.to_ascii_lowercase());
    let record = fonts.entry(key).or_insert_with(|| IdmlFont {
        faces: Vec::new(),
        filename: None,
        data: None,
        omitted_reason: Some(reason),
        sources: Vec::new(),
    });
    if !record.faces.contains(&face) {
        record.faces.push(face);
    }
    if !record.sources.iter().any(|existing| existing == source) {
        record.sources.push(source.to_owned());
    }
}

fn export(
    document: &PagedDocument,
    options: &IdmlOptions,
    diagnostics: &mut EcoVec<SourceDiagnostic>,
) -> Result<(Vec<(String, Vec<u8>)>, BTreeMap<String, IdmlFont>), EcoString> {
    let selected = document
        .pages()
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            options
                .pages
                .as_ref()
                .is_none_or(|ranges| ranges.includes_page_index(*index))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("IDML export selected no pages".into());
    }
    let master_size = selected[0].1.frame.size();

    let mut ids = Ids::default();
    let mut files = Vec::<(String, Vec<u8>)>::new();
    let mut stories = Vec::new();
    let mut story_ids = Vec::new();
    let mut colors = Vec::<(String, [u8; 3])>::new();
    let mut fonts = BTreeMap::new();
    let mut spread_refs = String::new();
    let mut exported_pages = Vec::with_capacity(selected.len());

    for (page_index, page) in selected {
        let page_number = page_index + 1;
        let size = page.frame.size();
        let page_id = ids.next();
        let page_transform = page_to_spread_transform(page_number, size);
        let mut page_items = String::new();

        // IDML pages are white by default. An explicit Typst page fill is an
        // independent vector item behind all authored content.
        if let Some(fill) = page.fill_or_transparent() {
            let shape = GeometryRect::new(size, fill.into());
            let mut frame = Frame::new(size, FrameKind::Hard);
            frame.push(Point::zero(), FrameItem::Shape(shape.0, Span::detached()));
            let viewport = Rect::from_pos_size(Point::zero(), size);
            let svg = typst_svg::try_svg_frame_with_viewport(
                &frame,
                size,
                viewport,
                page_number,
            )
            .map_err(|error| {
                format!("página {page_number}: no se pudo exportar el fondo: {error}")
            })?;
            for warning in svg.warnings {
                diagnostics.push(SourceDiagnostic::warning(Span::detached(), warning));
            }
            let file = format!("Links/Graphic_{}.svg", ids.counter);
            files.push((file.clone(), svg.svg.into_bytes()));
            page_items.push_str(&graphic_frame_xml(
                ids.next(),
                ids.next(),
                ids.next(),
                "SVG",
                &file,
                size,
                size,
                page_transform,
            ));
        }

        let mut objects = Vec::new();
        collect_items(&page.frame, size, Transform::identity(), &[], &mut objects);

        for object in objects {
            match object {
                Object::Text { text, transform } => {
                    let story_id = ids.next();
                    let text_frame_id = ids.next();
                    let story = story_xml(&story_id, &text, &mut colors, &mut fonts);
                    stories.push((story_id.clone(), story));
                    story_ids.push(story_id.clone());
                    page_items.push_str(&text_frame_xml(
                        &text_frame_id,
                        &story_id,
                        &text,
                        page_transform.pre_concat(transform),
                    ));
                }
                Object::Raster { image, size: item_size, transform, label } => {
                    let raster = match image.kind() {
                        ImageKind::Raster(raster) => raster,
                        _ => unreachable!("raster object carries a raster image"),
                    };
                    let mut encoded = Cursor::new(Vec::new());
                    raster
                        .dynamic()
                        .write_to(&mut encoded, ImageFormat::Png)
                        .map_err(|error| {
                            format!(
                                "página {page_number}: no se pudo empaquetar la imagen «{label}»: {error}"
                            )
                        })?;
                    let file = format!("Links/Image_{}.png", ids.counter);
                    files.push((file.clone(), encoded.into_inner()));
                    let source_size = Size::new(
                        Abs::pt(f64::from(raster.width())),
                        Abs::pt(f64::from(raster.height())),
                    );
                    page_items.push_str(&graphic_frame_xml(
                        ids.next(),
                        ids.next(),
                        ids.next(),
                        "PNG",
                        &file,
                        item_size,
                        source_size,
                        page_transform.pre_concat(transform),
                    ));
                }
                Object::Vector { frame, viewport, label } => {
                    let svg = typst_svg::try_svg_frame_with_viewport_for_idml(
                        &frame, size, viewport, page_number,
                    )
                    .map_err(|error| {
                        format!(
                            "página {page_number}: no se pudo exportar el recurso «{label}»: {error}"
                        )
                    })?;
                    for warning in svg.warnings {
                        diagnostics
                            .push(SourceDiagnostic::warning(Span::detached(), warning));
                    }
                    let asset_size = viewport.size();
                    let file = format!("Links/Graphic_{}.svg", ids.counter);
                    files.push((file.clone(), svg.svg.into_bytes()));
                    let transform = page_transform
                        .pre_concat(Transform::translate(viewport.min.x, viewport.min.y));
                    page_items.push_str(&graphic_frame_xml(
                        ids.next(),
                        ids.next(),
                        ids.next(),
                        "SVG",
                        &file,
                        asset_size,
                        asset_size,
                        transform,
                    ));
                }
            }
        }

        exported_pages.push(ExportedPage {
            id: page_id,
            number: page_number,
            size,
            transform: page_transform,
            items: page_items,
        });
    }

    for (spread_index, pages) in
        facing_page_groups(exported_pages).into_iter().enumerate()
    {
        let spread_id = format!("uSpread{}", spread_index + 1);
        let spread = spread_xml(&spread_id, &pages);
        files.push((format!("Spreads/Spread_{spread_id}.xml"), spread));
        spread_refs
            .push_str(&format!("<idPkg:Spread src=\"Spreads/Spread_{spread_id}.xml\"/>"));
    }

    files.push(("Resources/Graphic.xml".into(), graphic_resources_xml(&colors)));
    files.push((
        "Resources/Preferences.xml".into(),
        document_preferences_xml(master_size),
    ));
    files.push(("Resources/Fonts.xml".into(), resource_root("Fonts")));
    files.push(("Resources/Styles.xml".into(), styles_xml()));
    files.push(("XML/Tags.xml".into(), tags_xml()));
    files.push(("XML/Mapping.xml".into(), mapping_xml()));
    files.push(("XML/BackingStory.xml".into(), backing_story_xml()));
    files.push((
        "MasterSpreads/MasterSpread_uMaster.xml".into(),
        master_spread_xml(master_size),
    ));

    for (story_id, xml) in stories {
        files.push((format!("Stories/Story_{story_id}.xml"), xml));
    }
    let story_refs = story_ids
        .iter()
        .map(|id| format!("<idPkg:Story src=\"Stories/Story_{id}.xml\"/>",))
        .collect::<String>();
    files.push((
        "designmap.xml".into(),
        designmap_xml(&story_ids, &spread_refs, &story_refs),
    ));
    files.push(("META-INF/container.xml".into(), container_xml()));
    files.push(("META-INF/metadata.xml".into(), metadata_xml()));

    Ok((files, fonts))
}

#[derive(Default)]
struct Ids {
    counter: usize,
}

impl Ids {
    fn next(&mut self) -> String {
        self.counter += 1;
        format!("u{}", self.counter)
    }
}

#[derive(Clone)]
enum Object {
    Text { text: TextItem, transform: Transform },
    Raster { image: Image, size: Size, transform: Transform, label: String },
    Vector { frame: Frame, viewport: Rect, label: String },
}

struct ExportedPage {
    id: String,
    number: usize,
    size: Size,
    transform: Transform,
    items: String,
}

/// Map Typst page coordinates into an InDesign spread. Odd-numbered pages are
/// recto pages on the right; even-numbered pages are verso pages on the left.
fn page_to_spread_transform(page_number: usize, size: Size) -> Transform {
    let x = if page_number.is_multiple_of(2) { -size.x } else { Abs::zero() };
    Transform::translate(x, Abs::pt(-size.y.to_pt() / 2.0))
}

/// Put selected source pages into their natural book spreads. Page 1 remains
/// alone as the cover. The standard book pairs are 2–3, 4–5, and so on; pages
/// selected without their partner remain single pages on their original side.
fn facing_page_groups(pages: Vec<ExportedPage>) -> Vec<Vec<ExportedPage>> {
    let mut pages = pages.into_iter().peekable();
    let mut groups = Vec::new();
    while let Some(page) = pages.next() {
        if page.number.is_multiple_of(2)
            && pages.peek().is_some_and(|next| next.number == page.number + 1)
        {
            groups.push(vec![page, pages.next().unwrap()]);
        } else {
            groups.push(vec![page]);
        }
    }
    groups
}

#[derive(Clone)]
struct Ancestor {
    parent_size: Size,
    parent_kind: FrameKind,
    position: Point,
    group: GroupItem,
}

fn collect_items(
    frame: &Frame,
    canvas_size: Size,
    parent_transform: Transform,
    ancestors: &[Ancestor],
    out: &mut Vec<Object>,
) {
    for (position, item) in frame.items() {
        let transform =
            parent_transform.pre_concat(Transform::translate(position.x, position.y));
        match item {
            FrameItem::Group(group) => {
                let mut path = ancestors.to_vec();
                path.push(Ancestor {
                    parent_size: frame.size(),
                    parent_kind: frame.kind(),
                    position: *position,
                    group: group.clone(),
                });
                collect_items(
                    &group.frame,
                    canvas_size,
                    transform.pre_concat(group.transform),
                    &path,
                    out,
                );
            }
            FrameItem::Text(text) if typst_svg::can_render_live_text(text) => {
                out.push(Object::Text { text: text.clone(), transform });
            }
            FrameItem::Text(text) => {
                let local = text.bbox();
                if let Some(viewport) = transformed_rect(local, transform) {
                    out.push(Object::Vector {
                        frame: isolated_frame(frame, *position, item.clone(), ancestors),
                        viewport,
                        label: format!("texto «{}» convertido a contornos", text.text),
                    });
                }
            }
            FrameItem::Shape(shape, _) => {
                let local = shape.bbox(true);
                if let Some(viewport) = transformed_rect(local, transform) {
                    out.push(Object::Vector {
                        frame: isolated_frame(frame, *position, item.clone(), ancestors),
                        viewport,
                        label: "trazado Typst".into(),
                    });
                }
            }
            FrameItem::Image(image, item_size, _) => {
                let local = Rect::from_pos_size(Point::zero(), *item_size);
                if !ancestors.iter().any(|ancestor| ancestor.group.clip.is_some())
                    && let ImageKind::Raster(_) = image.kind()
                {
                    out.push(Object::Raster {
                        image: image.clone(),
                        size: *item_size,
                        transform,
                        label: image.alt().unwrap_or("imagen raster").to_owned(),
                    });
                } else if let Some(viewport) = transformed_rect(local, transform) {
                    out.push(Object::Vector {
                        frame: isolated_frame(frame, *position, item.clone(), ancestors),
                        viewport,
                        label: image.alt().unwrap_or("gráfico colocado").to_owned(),
                    });
                }
            }
            FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
        }
    }
    let _ = canvas_size;
}

fn isolated_frame(
    leaf_parent: &Frame,
    position: Point,
    item: FrameItem,
    ancestors: &[Ancestor],
) -> Frame {
    let mut current = Frame::new(leaf_parent.size(), leaf_parent.kind());
    current.push(position, item);
    for ancestor in ancestors.iter().rev() {
        let mut group = ancestor.group.clone();
        group.frame = current;
        let mut parent = Frame::new(ancestor.parent_size, ancestor.parent_kind);
        parent.push(ancestor.position, FrameItem::Group(group));
        current = parent;
    }
    current
}

fn transformed_rect(rect: Rect, transform: Transform) -> Option<Rect> {
    let corners = [
        rect.min,
        Point::new(rect.max.x, rect.min.y),
        rect.max,
        Point::new(rect.min.x, rect.max.y),
    ]
    .map(|point| point.transform(transform));
    let mut min = Point::splat(Abs::inf());
    let mut max = Point::splat(-Abs::inf());
    for point in corners {
        min = min.min(point);
        max = max.max(point);
    }
    let result = Rect::new(min, max);
    (result.size().is_finite()
        && result.size().x > Abs::zero()
        && result.size().y > Abs::zero())
    .then_some(result)
}

fn text_frame_xml(
    frame_id: &str,
    story_id: &str,
    text: &TextItem,
    transform: Transform,
) -> String {
    let size = text.size.to_pt();
    let metrics = text.font.metrics();
    let ascender = metrics.ascender.at(text.size).to_pt().max(size * 0.6);
    let descender = (-metrics.descender).at(text.size).to_pt().max(size * 0.2);
    let bbox = text.bbox();
    let glyph_right = if bbox.max.x.is_finite() { bbox.max.x.to_pt() } else { 0.0 };
    let width = text.width().to_pt().max(glyph_right).max(size * 0.5) + 0.1;
    let bounds = Rect::new(
        Point::new(Abs::zero(), Abs::pt(-ascender)),
        Point::new(Abs::pt(width), Abs::pt(descender)),
    );
    let mut xml = format!(
        "<TextFrame Self=\"{}\" ParentStory=\"{}\" PreviousTextFrame=\"n\" NextTextFrame=\"n\" ContentType=\"TextType\" ItemLayer=\"uLayer\" AppliedObjectStyle=\"ObjectStyle/$ID/[None]\" Visible=\"true\" ItemTransform=\"{}\" FillColor=\"Swatch/None\" StrokeColor=\"Swatch/None\" StrokeWeight=\"0\">",
        escape(frame_id),
        escape(story_id),
        matrix(transform),
    );
    xml.push_str(&path_geometry(bounds));
    xml.push_str(&format!(
        "<TextFramePreference FirstBaselineOffset=\"Fixed\" MinimumFirstBaselineOffset=\"{ascender:.4}\" InsetSpacing=\"0 0 0 0\" VerticalJustification=\"TopAlign\" AutoSizingType=\"HeightAndWidth\" AutoSizingReferencePoint=\"TopLeftPoint\" UseNoLineBreaksForAutoSizing=\"true\" />"
    ));
    xml.push_str("</TextFrame>");
    xml
}

fn graphic_frame_xml(
    frame_id: String,
    image_id: String,
    link_id: String,
    kind: &str,
    link_path: &str,
    size: Size,
    source_size: Size,
    transform: Transform,
) -> String {
    let path = Rect::from_pos_size(Point::zero(), size);
    let (description, type_name) = if kind == "PNG" {
        ("$ID/PNG", "$ID/Portable Network Graphics (PNG)")
    } else {
        ("$ID/SVG", "$ID/SVG")
    };
    let image_transform = Transform::scale(
        Ratio::new(size.x.to_pt() / source_size.x.to_pt()),
        Ratio::new(size.y.to_pt() / source_size.y.to_pt()),
    );
    let uri = format!("file:{link_path}");
    format!(
        "<Rectangle Self=\"{}\" ContentType=\"GraphicType\" ItemLayer=\"uLayer\" AppliedObjectStyle=\"ObjectStyle/$ID/[None]\" Visible=\"true\" ItemTransform=\"{}\" FillColor=\"Swatch/None\" StrokeColor=\"Swatch/None\" StrokeWeight=\"0\">{}<Image Self=\"{}\" ImageTypeName=\"{}\" ItemTransform=\"{}\" GeometricBounds=\"0 0 {:.4} {:.4}\"><Properties><GraphicBounds Left=\"0\" Top=\"0\" Right=\"{:.4}\" Bottom=\"{:.4}\" /></Properties><Link Self=\"{}\" AssetURL=\"$ID/\" AssetID=\"$ID/\" LinkResourceURI=\"{}\" LinkResourceFormat=\"{}\" StoredState=\"Normal\" LinkResourceModified=\"false\" LinkObjectModified=\"false\" ShowInUI=\"true\" CanEmbed=\"true\" CanUnembed=\"true\" CanPackage=\"true\" ImportPolicy=\"NoAutoImport\" ExportPolicy=\"NoAutoExport\" /></Image></Rectangle>",
        escape(&frame_id),
        matrix(transform),
        path_geometry(path),
        escape(&image_id),
        type_name,
        matrix(image_transform),
        source_size.y.to_pt(),
        source_size.x.to_pt(),
        source_size.x.to_pt(),
        source_size.y.to_pt(),
        escape(&link_id),
        escape(&uri),
        description,
    )
}

fn spread_xml(spread_id: &str, pages: &[ExportedPage]) -> Vec<u8> {
    let binding_location =
        if pages.len() == 2 || pages[0].number.is_multiple_of(2) { 1 } else { 0 };
    let mut page_elements = String::new();
    let mut items = String::new();
    for page in pages {
        page_elements.push_str(&format!(
            "<Page Self=\"{}\" Name=\"{}\" AppliedMaster=\"uMaster\" ItemTransform=\"{}\" GeometricBounds=\"0 0 {:.4} {:.4}\" MasterPageTransform=\"1 0 0 1 0 0\" />",
            escape(&page.id),
            page.number,
            matrix(page.transform),
            page.size.y.to_pt(),
            page.size.x.to_pt(),
        ));
        items.push_str(&page.items);
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Spread xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><Spread Self=\"{}\" PageCount=\"{}\" BindingLocation=\"{}\" ShowMasterItems=\"true\" AllowPageShuffle=\"false\" ItemTransform=\"1 0 0 1 0 0\">{}{}</Spread></idPkg:Spread>",
        escape(spread_id),
        pages.len(),
        binding_location,
        page_elements,
        items,
    )
    .into_bytes()
}

fn story_xml(
    story_id: &str,
    text: &TextItem,
    colors: &mut Vec<(String, [u8; 3])>,
    fonts: &mut BTreeMap<String, IdmlFont>,
) -> Vec<u8> {
    let (fill, color) = color_ref(&text.fill);
    if let Some(color) = color {
        if !colors.iter().any(|(_, rgb)| *rgb == color) {
            colors.push((fill.trim_start_matches("Color/").to_owned(), color));
        }
    }
    let variant = text.font.font().info().variant;
    let fallback_style = font_style_name(variant.style, variant.weight.to_number());
    let (font_family, font_style) = idml_font_names(text.font.font(), &fallback_style);
    add_font(text.font.font(), &font_family, &font_style, None, fonts);
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Story xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><Story Self=\"{}\" StoryDirection=\"LeftToRightDirection\"><ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/$ID/[No paragraph style]\"><CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/$ID/[No character style]\" PointSize=\"{:.4}\" FillColor=\"{}\"",
        escape(story_id),
        text.size.to_pt(),
        escape(&fill),
    );
    if let Some(stroke) = &text.stroke {
        if let Paint::Solid(color) = &stroke.paint {
            let color = rgb(color);
            let name = format!("text-{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
            if !colors.iter().any(|(existing, _)| existing == &name) {
                colors.push((name.clone(), color));
            }
            xml.push_str(&format!(
                " StrokeColor=\"Color/{name}\" StrokeWeight=\"{:.4}\"",
                stroke.thickness.to_pt()
            ));
        }
    }
    xml.push_str(&format!(
        "><Properties><AppliedFont type=\"string\">{}</AppliedFont><FontStyle type=\"string\">{}</FontStyle></Properties><Content>{}</Content></CharacterStyleRange></ParagraphStyleRange></Story></idPkg:Story>",
        escape(&font_family), escape(&font_style), escape(&text.text),
    ));
    xml.into_bytes()
}

fn add_font(
    font: &Font,
    family: &str,
    style: &str,
    source: Option<&Path>,
    fonts: &mut BTreeMap<String, IdmlFont>,
) {
    let data = font.data().as_ref();
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let digest = hasher.finish();
    let key = format!("{digest:016x}");
    let face = format!("{family} — {style}");

    let record = fonts.entry(key.clone()).or_insert_with(|| {
        let extension = font_extension(data);
        let omitted_reason = if extension.is_none() {
            Some(
                "Typst font format is unsupported in the InDesign font folder".to_owned(),
            )
        } else {
            font_package_omission(data)
        };
        let filename = if omitted_reason.is_none() {
            let postscript =
                font.post_script_name().unwrap_or_else(|| font.info().family.clone());
            let stem = safe_font_filename(&postscript);
            Some(PathBuf::from(format!("{stem}-{digest:016x}.{}", extension.unwrap())))
        } else {
            None
        };
        IdmlFont {
            faces: Vec::new(),
            filename,
            data: omitted_reason.is_none().then(|| data.to_vec()),
            omitted_reason,
            sources: Vec::new(),
        }
    });
    if !record.faces.contains(&face) {
        record.faces.push(face);
    }
    if let Some(source) = source
        && !record.sources.iter().any(|existing| existing == source)
    {
        record.sources.push(source.to_owned());
    }
}

/// Resolve the names InDesign uses to identify this specific font face.
///
/// Typst groups some faces under a cleaned-up family and derives common style
/// labels (for example, `Geller` / `Bold`). InDesign instead matches the
/// OpenType typographic family and subfamily names. These can differ, as in
/// Geller's `Geller` / `Headline Bold` face.
fn idml_font_names(font: &Font, fallback_style: &str) -> (String, String) {
    let face = ttf_parser::Face::parse(font.data(), font.index()).ok();
    let preferred_family = face
        .as_ref()
        .and_then(|face| open_type_name(face, name_id::TYPOGRAPHIC_FAMILY));
    let preferred_subfamily = if font.info().flags.contains(FontFlags::VARIABLE) {
        None
    } else {
        face.as_ref()
            .and_then(|face| open_type_name(face, name_id::TYPOGRAPHIC_SUBFAMILY))
    };

    resolve_idml_font_names(
        &font.info().family,
        fallback_style,
        preferred_family.as_deref(),
        preferred_subfamily.as_deref(),
    )
}

fn open_type_name(face: &ttf_parser::Face<'_>, id: u16) -> Option<String> {
    let names = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == id && name.is_unicode())
        .collect::<Vec<_>>();
    let name = names
        .iter()
        .find(|name| {
            name.platform_id == ttf_parser::PlatformId::Windows
                && name.language_id == 0x0409
        })
        .or_else(|| {
            names
                .iter()
                .find(|name| name.platform_id == ttf_parser::PlatformId::Windows)
        })
        .or_else(|| {
            names
                .iter()
                .find(|name| name.platform_id == ttf_parser::PlatformId::Unicode)
        })?;
    name.to_string().filter(|name| !name.trim().is_empty())
}

fn resolve_idml_font_names(
    fallback_family: &str,
    fallback_style: &str,
    preferred_family: Option<&str>,
    preferred_subfamily: Option<&str>,
) -> (String, String) {
    (
        preferred_family.unwrap_or(fallback_family).to_owned(),
        preferred_subfamily.unwrap_or(fallback_style).to_owned(),
    )
}

fn font_extension(data: &[u8]) -> Option<&'static str> {
    match data.get(..4)? {
        b"OTTO" | b"typ1" => Some("otf"),
        b"\0\x01\0\0" | b"true" => Some("ttf"),
        b"ttcf" => Some("ttc"),
        _ => None,
    }
}

fn font_package_omission(data: &[u8]) -> Option<String> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    for index in 0..count {
        let Some(face) = ttf_parser::Face::parse(data, index).ok() else {
            return Some("font licensing metadata could not be read".to_owned());
        };
        let Some(os2) = face.tables().os2.as_ref() else {
            return Some("font has no readable embedding permissions".to_owned());
        };
        match os2.permissions() {
            Some(
                ttf_parser::Permissions::Installable | ttf_parser::Permissions::Editable,
            ) => {}
            Some(ttf_parser::Permissions::PreviewAndPrint) => {
                return Some(
                    "font permits preview and printing, not editable use".to_owned(),
                );
            }
            Some(ttf_parser::Permissions::Restricted) => {
                return Some("font license restricts packaging".to_owned());
            }
            None => return Some("font licensing permissions are malformed".to_owned()),
        }
    }
    None
}

fn safe_font_filename(name: &str) -> String {
    let safe = name
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() || char == '-' { char } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if safe.is_empty() { "Font".to_owned() } else { safe }
}

fn font_style_name(style: FontStyle, weight: u16) -> String {
    let weight_name = match weight {
        0..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "Semibold",
        651..=800 => "Bold",
        _ => "Black",
    };
    match style {
        FontStyle::Normal => weight_name.to_owned(),
        FontStyle::Italic if weight_name == "Regular" => "Italic".to_owned(),
        FontStyle::Italic => format!("{weight_name} Italic"),
        FontStyle::Oblique if weight_name == "Regular" => "Oblique".to_owned(),
        FontStyle::Oblique => format!("{weight_name} Oblique"),
    }
}

fn color_ref(paint: &Paint) -> (String, Option<[u8; 3]>) {
    let Paint::Solid(color) = paint else {
        return ("Color/Black".into(), None);
    };
    let rgb = rgb(color);
    let id = format!("text-{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    (format!("Color/{id}"), Some(rgb))
}

fn rgb(color: &Color) -> [u8; 3] {
    let (r, g, b, _) = color.to_rgb().into_format::<u8, u8>().into_components();
    [r, g, b]
}

fn graphic_resources_xml(colors: &[(String, [u8; 3])]) -> Vec<u8> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Graphic xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><Color Self=\"Color/Black\" Name=\"Black\" Model=\"Process\" Space=\"CMYK\" ColorValue=\"0 0 0 100\"/><Color Self=\"Color/Paper\" Name=\"Paper\" Model=\"Process\" Space=\"CMYK\" ColorValue=\"0 0 0 0\"/><Swatch Self=\"Swatch/None\" Name=\"$ID/None\"/><Swatch Self=\"Swatch/Paper\" Name=\"$ID/Paper\" ColorValue=\"Color/Paper\"/><Swatch Self=\"Swatch/Black\" Name=\"$ID/Black\" ColorValue=\"Color/Black\"/>",
    );
    for (name, rgb) in colors {
        xml.push_str(&format!(
            "<Color Self=\"Color/{}\" Name=\"{}\" Model=\"Process\" Space=\"RGB\" ColorValue=\"{} {} {}\" />",
            escape(name), escape(name), rgb[0], rgb[1], rgb[2],
        ));
    }
    xml.push_str("</idPkg:Graphic>");
    xml.into_bytes()
}

fn styles_xml() -> Vec<u8> {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Styles xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><RootCharacterStyleGroup Self=\"uRootCharacterStyles\"><CharacterStyle Self=\"CharacterStyle/$ID/[No character style]\" Name=\"$ID/[No character style]\" /></RootCharacterStyleGroup><RootParagraphStyleGroup Self=\"uRootParagraphStyles\"><ParagraphStyle Self=\"ParagraphStyle/$ID/[No paragraph style]\" Name=\"$ID/[No paragraph style]\" /></RootParagraphStyleGroup><RootObjectStyleGroup Self=\"uRootObjectStyles\"><ObjectStyle Self=\"ObjectStyle/$ID/[None]\" Name=\"$ID/[None]\" /></RootObjectStyleGroup></idPkg:Styles>".to_vec()
}

fn document_preferences_xml(size: Size) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Preferences xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><DocumentPreference PageHeight=\"{:.4}\" PageWidth=\"{:.4}\" FacingPages=\"true\" DocumentBleedTopOffset=\"0\" DocumentBleedBottomOffset=\"0\" DocumentBleedInsideOrLeftOffset=\"0\" DocumentBleedOutsideOrRightOffset=\"0\" DocumentBleedUniformSize=\"true\" SlugTopOffset=\"0\" SlugBottomOffset=\"0\" SlugInsideOrLeftOffset=\"0\" SlugRightOrOutsideOffset=\"0\" DocumentSlugUniformSize=\"false\" PreserveLayoutWhenShuffling=\"true\" AllowPageShuffle=\"true\" OverprintBlack=\"true\" PageBinding=\"LeftToRight\" ColumnDirection=\"Horizontal\" ColumnGuideLocked=\"true\" MasterTextFrame=\"false\" SnippetImportUsesOriginalLocation=\"false\" Intent=\"PrintIntent\" /></idPkg:Preferences>",
        size.y.to_pt(),
        size.x.to_pt(),
    )
    .into_bytes()
}

fn resource_root(name: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:{name} xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\" />").into_bytes()
}

fn tags_xml() -> Vec<u8> {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Tags xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><Tags Self=\"uTags\" /></idPkg:Tags>".to_vec()
}

fn mapping_xml() -> Vec<u8> {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:Mapping xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\" />".to_vec()
}

fn backing_story_xml() -> Vec<u8> {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:BackingStory xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><Story Self=\"uBackingStory\" /></idPkg:BackingStory>".to_vec()
}

fn master_spread_xml(size: Size) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><idPkg:MasterSpread xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\"><MasterSpread Self=\"uMaster\" Name=\"A-Master\" NamePrefix=\"A\" BaseName=\"Master\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\"><Page Self=\"uMasterPage\" Name=\"A\" AppliedMaster=\"n\" GeometricBounds=\"0 0 {:.4} {:.4}\" ItemTransform=\"1 0 0 1 0 0\" MasterPageTransform=\"1 0 0 1 0 0\" /></MasterSpread></idPkg:MasterSpread>",
        size.y.to_pt(),
        size.x.to_pt(),
    )
    .into_bytes()
}

fn designmap_xml(story_ids: &[String], spread_refs: &str, story_refs: &str) -> Vec<u8> {
    let story_list = story_ids.join(" ");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><?aid style=\"50\" type=\"document\" readerVersion=\"6.0\" featureSet=\"257\"?><Document xmlns:idPkg=\"http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging\" DOMVersion=\"20.0\" Self=\"d\" Name=\"Typst IDML export\" StoryList=\"{}\" ActiveLayer=\"uLayer\" RGBProfile=\"sRGB IEC61966-2.1\" CMYKProfile=\"U.S. Web Coated (SWOP) v2\" SolidColorIntent=\"UseColorSettings\" AfterBlendingIntent=\"UseColorSettings\" DefaultImageIntent=\"UseColorSettings\"><idPkg:Graphic src=\"Resources/Graphic.xml\"/><idPkg:Fonts src=\"Resources/Fonts.xml\"/><idPkg:Styles src=\"Resources/Styles.xml\"/><idPkg:Preferences src=\"Resources/Preferences.xml\"/><idPkg:Tags src=\"XML/Tags.xml\"/><Layer Self=\"uLayer\" Name=\"Layer 1\" Visible=\"true\" Locked=\"false\" IgnoreWrap=\"false\" ShowGuides=\"true\" LockGuides=\"false\" UI=\"true\" Expendable=\"true\" Printable=\"true\"/><idPkg:MasterSpread src=\"MasterSpreads/MasterSpread_uMaster.xml\"/>{}<idPkg:BackingStory src=\"XML/BackingStory.xml\"/>{}<idPkg:Mapping src=\"XML/Mapping.xml\"/></Document>",
        escape(&story_list), spread_refs, story_refs,
    )
    .into_bytes()
}

fn container_xml() -> Vec<u8> {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"designmap.xml\" media-type=\"application/vnd.adobe.indesign-idml-package\" /></rootfiles></container>".to_vec()
}

fn metadata_xml() -> Vec<u8> {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><?xpacket begin=\"﻿\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" /></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>".as_bytes().to_vec()
}

fn path_geometry(rect: Rect) -> String {
    let left = rect.min.x.to_pt();
    let top = rect.min.y.to_pt();
    let right = rect.max.x.to_pt();
    let bottom = rect.max.y.to_pt();
    let points = [(left, top), (right, top), (right, bottom), (left, bottom)];
    let mut xml = String::from(
        "<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>",
    );
    for (x, y) in points {
        xml.push_str(&format!(
            "<PathPointType Anchor=\"{x:.4} {y:.4}\" LeftDirection=\"{x:.4} {y:.4}\" RightDirection=\"{x:.4} {y:.4}\" />"
        ));
    }
    xml.push_str("</PathPointArray></GeometryPathType></PathGeometry></Properties>");
    xml
}

fn matrix(transform: Transform) -> String {
    format!(
        "{:.8} {:.8} {:.8} {:.8} {:.4} {:.4}",
        transform.sx.get(),
        transform.ky.get(),
        transform.kx.get(),
        transform.sy.get(),
        transform.tx.to_pt(),
        transform.ty.to_pt(),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn package(files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, EcoString> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let stored =
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer
        .start_file("mimetype", stored)
        .map_err(|error| format!("could not create IDML mimetype entry: {error}"))?;
    writer
        .write_all(b"application/vnd.adobe.indesign-idml-package")
        .map_err(|error| format!("could not write IDML mimetype entry: {error}"))?;

    let compressed =
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        writer
            .start_file(&name, compressed)
            .map_err(|error| format!("could not add {name} to IDML package: {error}"))?;
        writer.write_all(&data).map_err(|error| {
            format!("could not write {name} to IDML package: {error}")
        })?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("could not finish IDML package: {error}").into())
}

// Keeps the page fill on the exact same SVG rendering path as other vectors.
struct GeometryRect(Shape);

impl GeometryRect {
    fn new(size: Size, fill: Paint) -> Self {
        Self(typst_library::visualize::Geometry::Rect(size).filled(fill))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use zip::ZipArchive;

    #[test]
    fn package_is_zip_with_valid_internal_xml_and_no_external_links() {
        let files = vec![
            ("designmap.xml".to_owned(), designmap_xml(&[], "", "")),
            (
                "Spreads/Spread_uSpread1.xml".to_owned(),
                spread_xml(
                    "uSpread1",
                    &[ExportedPage {
                        id: "uPage1".to_owned(),
                        number: 1,
                        size: Size::new(Abs::pt(100.0), Abs::pt(200.0)),
                        transform: page_to_spread_transform(
                            1,
                            Size::new(Abs::pt(100.0), Abs::pt(200.0)),
                        ),
                        items: String::new(),
                    }],
                ),
            ),
            ("Resources/Graphic.xml".to_owned(), graphic_resources_xml(&[])),
        ];
        let bytes = package(files).unwrap();
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(zip.by_index(0).unwrap().name(), "mimetype");
        for index in 1..zip.len() {
            let mut file = zip.by_index(index).unwrap();
            if file.name().ends_with(".xml") {
                let mut data = String::new();
                file.read_to_string(&mut data).unwrap();
                roxmltree::Document::parse(&data).unwrap();
                assert!(!data.contains("file:/"));
            }
        }
    }

    #[test]
    fn text_story_escapes_xml_and_keeps_characters() {
        assert_eq!(escape("á & <x>"), "á &amp; &lt;x&gt;");
    }

    #[test]
    fn regular_italic_uses_the_in_design_face_name() {
        assert_eq!(font_style_name(FontStyle::Italic, 400), "Italic");
        assert_eq!(font_style_name(FontStyle::Italic, 700), "Bold Italic");
        assert_eq!(font_style_name(FontStyle::Oblique, 400), "Oblique");
    }
}
