use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use ecow::{EcoString, eco_format};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::{FontData, FontQuery, StandardFont};
use hayro_svg::{RenderCache, SvgRenderSettings};
use image::{ImageEncoder, codecs::png::PngEncoder};
use typst_library::foundations::{Bytes, Smart};
use typst_library::layout::{Abs, Axes};
use typst_library::visualize::{
    ExchangeFormat, Image, ImageKind, ImageScaling, PdfImage, RasterFormat, SvgImage,
};

use crate::write::{SvgElem, SvgTransform, SvgWrite};
use crate::{SVGRenderer, State};

impl SVGRenderer<'_> {
    /// Render an image element.
    pub(super) fn render_image(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        image: &Image,
        size: &Axes<Abs>,
    ) {
        if self.text_mode == crate::SvgTextMode::Hybrid
            && let ImageKind::Svg(source) = image.kind()
        {
            self.render_imported_svg(svg, state, image, source, size);
            return;
        }

        let url = WebImage::new(image).to_base64_url();
        let mut svg = svg.elem("image");
        if !state.transform.is_identity() {
            svg.attr("transform", SvgTransform(state.transform));
        }
        svg.attr("xlink:href", url.as_str());
        svg.attr("width", size.x.to_pt());
        svg.attr("height", size.y.to_pt());
        svg.attr("preserveAspectRatio", "none");
        if let Some(value) = convert_image_scaling(image.scaling()) {
            svg.attr_with("style", |attr| {
                attr.push_str("image-rendering: ");
                attr.push_str(value);
            });
        }
    }

    fn render_imported_svg(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        image: &Image,
        source: &SvgImage,
        size: &Axes<Abs>,
    ) {
        self.svg_image_count += 1;
        let prefix = format!("typst-svg-import-{}-", self.svg_image_count);
        match prepare_svg(
            source,
            &prefix,
            self.outline_imported_svg_text,
            self.omit_imported_svg_drop_shadows,
        ) {
            Ok(prepared) => {
                if prepared.removed_drop_shadows > 0 {
                    let resource = image.alt().unwrap_or("SVG importado");
                    self.warnings.push(eco_format!(
                        "página {}: se omitieron {} sombras SVG del recurso «{}» para conservar visibles sus vectores en InDesign",
                        self.page_number,
                        prepared.removed_drop_shadows,
                        resource,
                    ));
                }
                write_imported_svg(svg, state, size, &prepared);
            }
            Err(reason) => match rasterize_svg(source, state, size) {
                Ok(png) => {
                    let resource = image.alt().unwrap_or("SVG importado");
                    self.warnings.push(eco_format!(
                            "página {}: el recurso «{}» no se pudo integrar como SVG ({}) y se incrustó como PNG a 300 ppi",
                            self.page_number,
                            resource,
                            reason
                        ));
                    write_raster_fallback(svg, state, image, size, &png);
                }
                Err(fallback_error) => {
                    let resource = image.alt().unwrap_or("SVG importado");
                    self.error = Some(eco_format!(
                        "página {}: no se pudo integrar el recurso SVG «{}»: {}; también falló el respaldo PNG a 300 ppi: {}",
                        self.page_number,
                        resource,
                        reason,
                        fallback_error
                    ));
                }
            },
        }
    }
}

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

#[derive(Debug)]
struct PreparedSvg {
    container_id: String,
    view_box: String,
    namespaces: Vec<(String, String)>,
    root_attrs: Vec<(String, String)>,
    children: Vec<PreparedChild>,
    removed_drop_shadows: usize,
}

#[derive(Debug)]
struct PreparedNode {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<PreparedChild>,
}

#[derive(Debug)]
enum PreparedChild {
    Element(PreparedNode),
    Text(String),
}

/// Parse an imported SVG and prepare it for safe insertion into the page.
///
/// All IDs are namespaced per placement, and references are checked before any
/// XML is written so an unsafe asset can cleanly fall back to a raster image.
fn prepare_svg(
    source: &SvgImage,
    prefix: &str,
    outline_text: bool,
    omit_drop_shadows: bool,
) -> Result<PreparedSvg, EcoString> {
    let outlined;
    let decoded;
    let bytes = if outline_text {
        outlined = source.tree().to_string(&usvg::WriteOptions {
            preserve_text: false,
            indent: usvg::Indent::None,
            attributes_indent: usvg::Indent::None,
            ..Default::default()
        });
        outlined.as_bytes()
    } else if source.data().starts_with(&[0x1f, 0x8b]) {
        decoded = usvg::decompress_svgz(source.data())
            .map_err(|err| eco_format!("no se pudo descomprimir el SVG: {err}"))?;
        decoded.as_slice()
    } else {
        source.data().as_slice()
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| eco_format!("el contenido SVG no es UTF-8 válido"))?;
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(text, options)
        .map_err(|err| eco_format!("XML SVG no válido: {err}"))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg"
        || root.tag_name().namespace().is_some_and(|uri| uri != SVG_NAMESPACE)
    {
        return Err(eco_format!("la raíz XML no es un elemento SVG"));
    }

    let drop_shadow_filter_ids =
        if omit_drop_shadows { drop_shadow_filter_ids(root) } else { HashSet::new() };
    let removed_drop_shadows = count_drop_shadow_uses(root, &drop_shadow_filter_ids);

    let mut ids = HashMap::new();
    for node in root.descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("id") {
            if id.is_empty() || ids.contains_key(id) {
                return Err(eco_format!(
                    "el SVG contiene un identificador vacío o repetido"
                ));
            }
            if id
                .chars()
                .any(|ch| !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')))
            {
                return Err(eco_format!(
                    "el SVG contiene un identificador no seguro para sus referencias"
                ));
            }
            let renamed = format!("{prefix}{id}");
            ids.insert(id.to_owned(), renamed);
        }
    }

    let mut container_id = format!("{prefix}__container");
    while ids.values().any(|id| id == &container_id) {
        container_id.push('_');
    }
    let namespaces = namespace_map(root);
    let (root_attrs, children) = prepare_element(
        root,
        true,
        &ids,
        &namespaces,
        &container_id,
        &drop_shadow_filter_ids,
    )?;
    let view_box = root
        .attribute("viewBox")
        .or_else(|| root.attribute("viewbox"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let size = source.tree().size();
            format!("0 0 {} {}", size.width(), size.height())
        });

    Ok(PreparedSvg {
        container_id,
        view_box,
        namespaces,
        root_attrs,
        children,
        removed_drop_shadows,
    })
}

/// Find filters made entirely of `feDropShadow` primitives.
fn drop_shadow_filter_ids(root: roxmltree::Node<'_, '_>) -> HashSet<String> {
    root.descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "filter")
        .filter(|filter| {
            let mut primitives = filter.children().filter(|child| child.is_element());
            primitives
                .next()
                .is_some_and(|first| first.tag_name().name() == "feDropShadow")
                && primitives
                    .all(|primitive| primitive.tag_name().name() == "feDropShadow")
        })
        .filter_map(|filter| filter.attribute("id").map(str::to_owned))
        .collect()
}

fn count_drop_shadow_uses(
    root: roxmltree::Node<'_, '_>,
    filter_ids: &HashSet<String>,
) -> usize {
    root.descendants()
        .filter(|node| node.is_element())
        .filter(|node| {
            node.attribute("filter")
                .is_some_and(|value| references_drop_shadow_filter(value, filter_ids))
        })
        .count()
}

fn references_drop_shadow_filter(value: &str, filter_ids: &HashSet<String>) -> bool {
    let Some(reference) = value
        .trim()
        .strip_prefix("url(")
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    let reference = reference.trim().trim_matches(|ch| ch == '\'' || ch == '"');
    reference.strip_prefix('#').is_some_and(|id| filter_ids.contains(id))
}

fn namespace_map(root: roxmltree::Node<'_, '_>) -> Vec<(String, String)> {
    let mut uris = BTreeMap::new();
    uris.insert(SVG_NAMESPACE.to_owned(), String::new());
    uris.insert(XLINK_NAMESPACE.to_owned(), "xlink".to_owned());
    uris.insert(XML_NAMESPACE.to_owned(), "xml".to_owned());
    for node in root.descendants().filter(|node| node.is_element()) {
        if let Some(uri) = node.tag_name().namespace() {
            uris.entry(uri.to_owned()).or_insert_with(String::new);
        }
        for attr in node.attributes() {
            if let Some(uri) = attr.namespace() {
                if uri != XMLNS_NAMESPACE {
                    uris.entry(uri.to_owned()).or_insert_with(String::new);
                }
            }
        }
    }

    let mut index = 0;
    for (uri, prefix) in &mut uris {
        if prefix.is_empty() && uri != SVG_NAMESPACE {
            *prefix = format!("ns{index}");
            index += 1;
        }
    }

    uris.into_iter().map(|(uri, prefix)| (prefix, uri)).collect()
}

fn prepare_element(
    node: roxmltree::Node<'_, '_>,
    is_root: bool,
    ids: &HashMap<String, String>,
    namespaces: &[(String, String)],
    container_id: &str,
    drop_shadow_filter_ids: &HashSet<String>,
) -> Result<(Vec<(String, String)>, Vec<PreparedChild>), EcoString> {
    let mut attrs = Vec::new();
    for attr in node.attributes() {
        if attr.namespace() == Some(XMLNS_NAMESPACE) {
            continue;
        }
        let local_name = attr.name();
        if local_name == "filter"
            && references_drop_shadow_filter(attr.value(), drop_shadow_filter_ids)
        {
            continue;
        }
        if is_root
            && matches!(
                local_name,
                "width"
                    | "height"
                    | "x"
                    | "y"
                    | "viewBox"
                    | "viewbox"
                    | "preserveAspectRatio"
                    | "overflow"
            )
        {
            continue;
        }

        let name = qualified_name(attr.namespace(), local_name, namespaces);
        let value = if local_name == "id" {
            ids.get(attr.value())
                .cloned()
                .ok_or_else(|| eco_format!("no se pudo renombrar un identificador SVG"))?
        } else if local_name == "href" {
            rewrite_href(attr.value(), ids, node.tag_name().name() == "a")?
        } else if matches!(
            local_name,
            "aria-labelledby"
                | "aria-describedby"
                | "aria-owns"
                | "aria-controls"
                | "aria-flowto"
                | "aria-details"
                | "for"
        ) {
            rewrite_id_list(attr.value(), ids)?
        } else if matches!(local_name, "begin" | "end") {
            rewrite_smil_timing(attr.value(), ids)?
        } else {
            let mut value = rewrite_urls(attr.value(), ids)?;
            if is_root && local_name == "style" {
                value = force_root_overflow_hidden(&value);
            }
            value
        };
        attrs.push((name, value));
    }

    let mut prepared_children = Vec::new();
    for child in node.children() {
        match child.node_type() {
            roxmltree::NodeType::Element => {
                if child.tag_name().name() == "filter"
                    && child
                        .attribute("id")
                        .is_some_and(|id| drop_shadow_filter_ids.contains(id))
                {
                    continue;
                }
                let name = qualified_name(
                    child.tag_name().namespace(),
                    child.tag_name().name(),
                    namespaces,
                );
                let (attrs, children) = prepare_element(
                    child,
                    false,
                    ids,
                    namespaces,
                    container_id,
                    drop_shadow_filter_ids,
                )?;
                prepared_children.push(PreparedChild::Element(PreparedNode {
                    name,
                    attrs,
                    children,
                }));
            }
            roxmltree::NodeType::Text => {
                let value = child.text().unwrap_or_default();
                let value = if node.tag_name().name() == "style" {
                    rewrite_css(value, ids, container_id)?
                } else {
                    value.to_owned()
                };
                if !value.is_empty() {
                    prepared_children.push(PreparedChild::Text(value));
                }
            }
            roxmltree::NodeType::Comment => {}
            roxmltree::NodeType::PI => {
                return Err(eco_format!(
                    "el SVG contiene instrucciones de procesamiento XML"
                ));
            }
            roxmltree::NodeType::Root => unreachable!(),
        }
    }
    Ok((attrs, prepared_children))
}

fn qualified_name(
    namespace: Option<&str>,
    local: &str,
    namespaces: &[(String, String)],
) -> String {
    let Some(namespace) = namespace else {
        return local.to_owned();
    };
    if namespace == SVG_NAMESPACE {
        return local.to_owned();
    }
    let prefix = namespaces
        .iter()
        .find(|(_, uri)| uri == namespace)
        .map(|(prefix, _)| prefix.as_str())
        .unwrap_or("ns-unknown");
    format!("{prefix}:{local}")
}

fn rewrite_href(
    value: &str,
    ids: &HashMap<String, String>,
    is_link: bool,
) -> Result<String, EcoString> {
    if let Some(id) = value.strip_prefix('#') {
        return ids
            .get(id)
            .map(|renamed| format!("#{renamed}"))
            .ok_or_else(|| eco_format!("referencia local sin destino: #{id}"));
    }
    if value.starts_with("data:") {
        if value.to_ascii_lowercase().starts_with("data:image/svg+xml") {
            return Err(eco_format!("el SVG contiene otro SVG codificado como imagen"));
        }
        return Ok(value.to_owned());
    }
    if is_link && !value.is_empty() {
        return Ok(value.to_owned());
    }
    if value.is_empty() {
        return Ok(value.to_owned());
    }
    Err(eco_format!("referencia externa no integrable: {value}"))
}

fn rewrite_smil_timing(
    value: &str,
    ids: &HashMap<String, String>,
) -> Result<String, EcoString> {
    value
        .split(';')
        .map(|entry| {
            let trimmed = entry.trim();
            let Some((id, rest)) = trimmed.split_once('.') else {
                return Ok(entry.to_owned());
            };
            if let Some(renamed) = ids.get(id) {
                let leading = entry.len() - entry.trim_start().len();
                Ok(format!("{}{}.{rest}", &entry[..leading], renamed))
            } else {
                Err(eco_format!("referencia temporal local sin destino: #{id}"))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(";"))
}

fn rewrite_id_list(
    value: &str,
    ids: &HashMap<String, String>,
) -> Result<String, EcoString> {
    value
        .split_whitespace()
        .map(|id| {
            ids.get(id)
                .cloned()
                .ok_or_else(|| eco_format!("referencia local sin destino: #{id}"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|ids| ids.join(" "))
}

fn rewrite_urls(value: &str, ids: &HashMap<String, String>) -> Result<String, EcoString> {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(index) = remaining.to_ascii_lowercase().find("url(") {
        output.push_str(&remaining[..index]);
        let after_url = &remaining[index + 4..];
        let Some(end) = after_url.find(')') else {
            return Err(eco_format!("referencia CSS url() incompleta"));
        };
        let raw = after_url[..end].trim();
        let target = raw
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .unwrap_or(raw);
        let rewritten = if let Some(id) = target.strip_prefix('#') {
            let renamed = ids
                .get(id)
                .ok_or_else(|| eco_format!("referencia local sin destino: #{id}"))?;
            format!("url(#{renamed})")
        } else if target.starts_with("data:") {
            if target.to_ascii_lowercase().starts_with("data:image/svg+xml") {
                return Err(eco_format!(
                    "el SVG contiene otro SVG codificado como imagen"
                ));
            }
            format!("url({target})")
        } else {
            return Err(eco_format!("referencia CSS externa no integrable: {target}"));
        };
        output.push_str(&rewritten);
        remaining = &after_url[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn rewrite_css(
    value: &str,
    ids: &HashMap<String, String>,
    container_id: &str,
) -> Result<String, EcoString> {
    let lower = value.to_ascii_lowercase();
    if ["@import", "@font-face", "@keyframes"]
        .iter()
        .any(|rule| lower.contains(rule))
    {
        return Err(eco_format!(
            "el SVG contiene reglas CSS globales que no se pueden aislar"
        ));
    }
    let value = rewrite_urls(value, ids)?;
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    let bytes = value.as_bytes();
    while cursor < bytes.len() {
        if bytes[cursor] == b'{' {
            let start = cursor + 1;
            let Some(relative_end) = value[start..].find('}') else {
                return Err(eco_format!("bloque CSS incompleto"));
            };
            let end = start + relative_end;
            output.push_str(&value[cursor..=end]);
            cursor = end + 1;
            continue;
        }
        if bytes[cursor] == b'}' {
            output.push('}');
            cursor += 1;
            continue;
        }
        let Some(relative_open) = value[cursor..].find('{') else {
            output.push_str(&value[cursor..]);
            break;
        };
        let open = cursor + relative_open;
        let selector = &value[cursor..open];
        let selector = rewrite_css_selector(selector, ids)?;
        output.push_str(&scope_css_selector(&selector, container_id));
        output.push('{');
        cursor = open + 1;
    }
    Ok(output)
}

fn scope_css_selector(selector: &str, container_id: &str) -> String {
    let trimmed = selector.trim();
    if trimmed.starts_with('@') {
        return selector.to_owned();
    }

    let mut result = String::with_capacity(selector.len() + container_id.len() + 4);
    let mut start = 0;
    let mut depth = 0_u32;
    let mut quote = None;
    for (index, ch) in selector.char_indices() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                append_scoped_selector(
                    &mut result,
                    &selector[start..index],
                    container_id,
                );
                result.push(',');
                start = index + 1;
            }
            _ => {}
        }
    }
    append_scoped_selector(&mut result, &selector[start..], container_id);
    result
}

fn append_scoped_selector(output: &mut String, selector: &str, container_id: &str) {
    let leading = selector.len() - selector.trim_start().len();
    let value = selector.trim();
    output.push_str(&selector[..leading]);
    if value.starts_with(":root") {
        output.push('#');
        output.push_str(container_id);
        output.push_str(" > svg");
        output.push_str(&value[5..]);
    } else {
        output.push('#');
        output.push_str(container_id);
        output.push(' ');
        output.push_str(value);
    }
}

fn rewrite_css_selector(
    value: &str,
    ids: &HashMap<String, String>,
) -> Result<String, EcoString> {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_hash) = value[cursor..].find('#') {
        let hash = cursor + relative_hash;
        output.push_str(&value[cursor..hash + 1]);
        let start = hash + 1;
        let end = value[start..]
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')))
            .map(|(index, _)| start + index)
            .unwrap_or(value.len());
        if start == end {
            cursor = start;
            continue;
        }
        let id = &value[start..end];
        let renamed = ids.get(id).ok_or_else(|| {
            eco_format!("selector CSS con identificador sin destino: #{id}")
        })?;
        output.push_str(renamed);
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    Ok(output)
}

fn force_root_overflow_hidden(style: &str) -> String {
    let declarations = style
        .split(';')
        .filter(|declaration| {
            declaration.split_once(':').is_none_or(|(property, _)| {
                !property.trim().eq_ignore_ascii_case("overflow")
            })
        })
        .map(str::trim)
        .filter(|declaration| !declaration.is_empty())
        .collect::<Vec<_>>();
    if declarations.is_empty() {
        "overflow:hidden".to_owned()
    } else {
        format!("{};overflow:hidden", declarations.join(";"))
    }
}

fn write_imported_svg(
    parent: &mut SvgElem,
    state: &State,
    size: &Axes<Abs>,
    prepared: &PreparedSvg,
) {
    let mut group = parent.elem("g");
    group.attr("id", prepared.container_id.as_str());
    if !state.transform.is_identity() {
        group.attr("transform", SvgTransform(state.transform));
    }
    let mut nested = group.elem("svg");
    nested
        .attr("x", 0.0)
        .attr("y", 0.0)
        .attr("width", size.x.to_pt())
        .attr("height", size.y.to_pt())
        .attr("viewBox", prepared.view_box.as_str())
        .attr("preserveAspectRatio", "none")
        .attr("overflow", "hidden")
        .attr("xmlns", SVG_NAMESPACE);
    for (prefix, uri) in &prepared.namespaces {
        if prefix.is_empty() || prefix == "xml" {
            continue;
        }
        nested.attr(&format!("xmlns:{prefix}"), uri.as_str());
    }
    for (name, value) in &prepared.root_attrs {
        nested.attr(name, value.as_str());
    }
    for child in &prepared.children {
        write_prepared_child(&mut nested, child);
    }
}

fn write_prepared_child(parent: &mut SvgElem, child: &PreparedChild) {
    match child {
        PreparedChild::Element(child) => write_prepared_node(parent, child),
        PreparedChild::Text(text) => {
            parent.text(text);
        }
    }
}

fn write_prepared_node(parent: &mut SvgElem, node: &PreparedNode) {
    let mut element = parent.elem(&node.name);
    for (name, value) in &node.attrs {
        element.attr(name, value.as_str());
    }
    for child in &node.children {
        write_prepared_child(&mut element, child);
    }
}

fn rasterize_svg(
    source: &SvgImage,
    state: &State,
    size: &Axes<Abs>,
) -> Result<Bytes, EcoString> {
    let transform = state.transform;
    let scale_x = (transform.sx.get() * transform.sx.get()
        + transform.kx.get() * transform.kx.get())
    .sqrt()
    .max(1.0e-6);
    let scale_y = (transform.ky.get() * transform.ky.get()
        + transform.sy.get() * transform.sy.get())
    .sqrt()
    .max(1.0e-6);
    let width = (size.x.to_pt() * 300.0 / 72.0 * scale_x).ceil().max(1.0);
    let height = (size.y.to_pt() * 300.0 / 72.0 * scale_y).ceil().max(1.0);
    if !width.is_finite()
        || !height.is_finite()
        || width > u32::MAX as f64
        || height > u32::MAX as f64
    {
        return Err(eco_format!("las dimensiones raster exceden el límite permitido"));
    }
    let width = width as u32;
    let height = height as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
        eco_format!("no se pudo reservar un mapa de bits de {width}×{height}")
    })?;
    let tree = source.tree();
    let tree_size = tree.size();
    let scale = tiny_skia::Transform::from_scale(
        width as f32 / tree_size.width(),
        height as f32 / tree_size.height(),
    );
    resvg::render(tree, scale, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map(Bytes::new)
        .map_err(|err| eco_format!("no se pudo codificar el PNG: {err}"))
}

fn write_raster_fallback(
    svg: &mut SvgElem,
    state: &State,
    image: &Image,
    size: &Axes<Abs>,
    png: &Bytes,
) {
    let mut url = eco_format!("data:image/png;base64,");
    url.push_str(&base64::engine::general_purpose::STANDARD.encode(png));
    let mut element = svg.elem("image");
    if !state.transform.is_identity() {
        element.attr("transform", SvgTransform(state.transform));
    }
    element
        .attr("xlink:href", url.as_str())
        .attr("width", size.x.to_pt())
        .attr("height", size.y.to_pt())
        .attr("preserveAspectRatio", "none");
    if let Some(value) = convert_image_scaling(image.scaling()) {
        element.attr_with("style", |attr| {
            attr.push_str("image-rendering: ");
            attr.push_str(value);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst_library::layout::{Size, Transform};

    const VECTOR_ART: &str = r##"
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20" width="40" height="20">
          <defs>
            <filter id="shadow"><feGaussianBlur stdDeviation="1"/></filter>
            <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
              <path d="M0,0 L6,3 L0,6 z"/>
            </marker>
            <clipPath id="clip"><rect width="40" height="20"/></clipPath>
          </defs>
          <style>#label, path { filter: url(#shadow); }</style>
          <g clip-path="url(#clip)">
            <path id="line" d="M2 10 H36" marker-end="url(#arrow)"/>
            <text id="label" x="2" y="8">Vector label</text>
          </g>
        </svg>
    "##;

    fn svg_image(source: &str) -> SvgImage {
        SvgImage::new(Bytes::from_string(source.to_owned())).unwrap()
    }

    fn render_inline(source: &SvgImage, prefix: &str, transform: Transform) -> String {
        let prepared = prepare_svg(source, prefix, false, false).unwrap();
        let mut writer = xmlwriter::XmlWriter::new(xmlwriter::Options::default());
        let mut root = SvgElem::new(&mut writer, "svg");
        let state = State::new(Size::new(Abs::pt(100.0), Abs::pt(60.0)))
            .with_transform(transform);
        write_imported_svg(
            &mut root,
            &state,
            &Axes::new(Abs::pt(40.0), Abs::pt(20.0)),
            &prepared,
        );
        drop(root);
        writer.end_document()
    }

    #[test]
    fn imported_svg_keeps_text_defs_refs_viewport_and_transform() {
        let image = svg_image(VECTOR_ART);
        let output = render_inline(
            &image,
            "typst-svg-import-1-",
            Transform::translate(Abs::pt(3.0), Abs::pt(5.0)),
        );
        let document = roxmltree::Document::parse(&output).unwrap();
        let imported = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "svg"
                    && node.attribute("viewBox").is_some()
            })
            .unwrap();

        assert_eq!(imported.attribute("viewBox"), Some("0 0 40 20"));
        assert_eq!(imported.attribute("preserveAspectRatio"), Some("none"));
        assert!(
            document.descendants().any(|node| {
                node.is_element()
                    && node.tag_name().name() == "text"
                    && node.text() == Some("Vector label")
            }),
            "{output}"
        );
        for tag in ["defs", "filter", "marker", "clipPath"] {
            assert!(
                document
                    .descendants()
                    .any(|node| { node.is_element() && node.tag_name().name() == tag })
            );
        }
        let path = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "path"
                    && node.attribute("id") == Some("typst-svg-import-1-line")
            })
            .unwrap();
        assert_eq!(path.attribute("marker-end"), Some("url(#typst-svg-import-1-arrow)"));
        assert!(output.contains("transform="));

        let style = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "style")
            .unwrap()
            .text()
            .unwrap();
        assert!(
            style.contains("#typst-svg-import-1-__container #typst-svg-import-1-label")
        );
        assert!(style.contains("url(#typst-svg-import-1-shadow)"));
    }

    #[test]
    fn repeated_imports_receive_disjoint_ids_and_references() {
        let image = svg_image(VECTOR_ART);
        let first = prepare_svg(&image, "typst-svg-import-1-", false, false).unwrap();
        let second = prepare_svg(&image, "typst-svg-import-2-", false, false).unwrap();
        let first_markup =
            render_inline(&image, "typst-svg-import-1-", Transform::identity());
        let second_markup =
            render_inline(&image, "typst-svg-import-2-", Transform::identity());

        assert_ne!(first.container_id, second.container_id);
        assert!(first_markup.contains("typst-svg-import-1-shadow"));
        assert!(second_markup.contains("typst-svg-import-2-shadow"));
        assert!(!second_markup.contains("typst-svg-import-1-shadow"));
        assert!(first_markup.contains("typst-svg-import-1-arrow"));
        assert!(second_markup.contains("typst-svg-import-2-arrow"));
    }

    #[test]
    fn missing_local_references_are_rejected_for_raster_fallback() {
        let image = svg_image(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect filter="url(#missing)"/></svg>"##,
        );
        let error = prepare_svg(&image, "typst-svg-import-1-", false, false).unwrap_err();
        assert!(error.contains("referencia local sin destino"));
    }

    #[test]
    fn unsafe_svg_can_be_rasterized_at_300_ppi() {
        let source = svg_image(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 5"><rect width="10" height="5" fill="red"/><image href="https://example.invalid/linked.png" width="1" height="1"/></svg>"##,
        );
        assert!(prepare_svg(&source, "typst-svg-import-1-", false, false).is_err());

        let state = State::new(Size::new(Abs::pt(72.0), Abs::pt(36.0)));
        let png =
            rasterize_svg(&source, &state, &Axes::new(Abs::pt(72.0), Abs::pt(36.0)))
                .unwrap();
        let decoded =
            image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (300, 150));

        let image =
            Image::new(source, Some(EcoString::from("enlace externo")), Smart::Auto);
        let mut renderer = crate::SVGRenderer::with_text_mode(crate::SvgTextMode::Hybrid);
        renderer.page_number = 7;
        let mut writer = xmlwriter::XmlWriter::new(xmlwriter::Options::default());
        let mut root = SvgElem::new(&mut writer, "svg");
        renderer.render_image(
            &mut root,
            &state,
            &image,
            &Axes::new(Abs::pt(72.0), Abs::pt(36.0)),
        );
        assert!(renderer.error.is_none());
        assert!(renderer.warnings[0].contains("página 7"));
        assert!(renderer.warnings[0].contains("enlace externo"));
        assert!(renderer.warnings[0].contains("300 ppi"));
        drop(root);
        assert!(writer.end_document().contains("data:image/png;base64,"));
    }
}

/// Converts an image scaling to a CSS `image-rendering` property value.
pub fn convert_image_scaling(scaling: Smart<ImageScaling>) -> Option<&'static str> {
    match scaling {
        Smart::Auto => None,
        Smart::Custom(ImageScaling::Smooth) => {
            // This is still experimental and not implemented in all major browsers.
            // https://developer.mozilla.org/en-US/docs/Web/CSS/image-rendering#browser_compatibility
            Some("smooth")
        }
        Smart::Custom(ImageScaling::Pixelated) => Some("pixelated"),
    }
}

/// An image that is prepared for use in the web platform (SVG or HTML).
#[derive(Debug, Clone, Hash)]
pub struct WebImage {
    pub format: WebImageFormat,
    pub data: Bytes,
}

/// An image format supported on the web.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum WebImageFormat {
    Png,
    Jpg,
    Gif,
    Webp,
    Svg,
}

impl WebImageFormat {
    /// The mime type for this format.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }

    /// The canonical extension used for this format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }
}

impl WebImage {
    /// Prepare an image for embedding in SVG or HTML.
    #[comemo::memoize]
    pub fn new(image: &Image) -> WebImage {
        let (format, data) = match image.kind() {
            ImageKind::Raster(raster) => match raster.format() {
                RasterFormat::Exchange(format) => (
                    match format {
                        ExchangeFormat::Png => WebImageFormat::Png,
                        ExchangeFormat::Jpg => WebImageFormat::Jpg,
                        ExchangeFormat::Gif => WebImageFormat::Gif,
                        ExchangeFormat::Webp => WebImageFormat::Webp,
                    },
                    raster.data().clone(),
                ),
                RasterFormat::Pixel(_) => (WebImageFormat::Png, {
                    let mut buf = vec![];
                    let mut encoder = PngEncoder::new(&mut buf);
                    if let Some(icc_profile) = raster.icc() {
                        encoder.set_icc_profile(icc_profile.to_vec()).ok();
                    }
                    raster.dynamic().write_with_encoder(encoder).unwrap();
                    Bytes::new(buf)
                }),
            },
            ImageKind::Svg(svg) => (WebImageFormat::Svg, svg.data().clone()),
            ImageKind::Pdf(pdf) => {
                (WebImageFormat::Svg, Bytes::from_string(pdf_to_svg(pdf)))
            }
        };
        Self { format, data }
    }

    /// Turns the image into a base64 URL.
    ///
    /// The format of the URL is `data:{mime};base64,`.
    #[comemo::memoize]
    pub fn to_base64_url(&self) -> EcoString {
        let mut url = eco_format!("data:{};base64,", self.format.mime());
        let data = base64::engine::general_purpose::STANDARD.encode(&self.data);
        url.push_str(&data);
        url
    }
}

// Keep this in sync with `typst-png`!
fn pdf_to_svg(pdf: &PdfImage) -> String {
    let select_standard_font = move |font: StandardFont| -> Option<(FontData, u32)> {
        let bytes = match font {
            StandardFont::Helvetica => typst_assets::pdf::SANS,
            StandardFont::HelveticaBold => typst_assets::pdf::SANS_BOLD,
            StandardFont::HelveticaOblique => typst_assets::pdf::SANS_ITALIC,
            StandardFont::HelveticaBoldOblique => typst_assets::pdf::SANS_BOLD_ITALIC,
            StandardFont::Courier => typst_assets::pdf::FIXED,
            StandardFont::CourierBold => typst_assets::pdf::FIXED_BOLD,
            StandardFont::CourierOblique => typst_assets::pdf::FIXED_ITALIC,
            StandardFont::CourierBoldOblique => typst_assets::pdf::FIXED_BOLD_ITALIC,
            StandardFont::TimesRoman => typst_assets::pdf::SERIF,
            StandardFont::TimesBold => typst_assets::pdf::SERIF_BOLD,
            StandardFont::TimesItalic => typst_assets::pdf::SERIF_ITALIC,
            StandardFont::TimesBoldItalic => typst_assets::pdf::SERIF_BOLD_ITALIC,
            StandardFont::ZapfDingBats => typst_assets::pdf::DING_BATS,
            StandardFont::Symbol => typst_assets::pdf::SYMBOL,
        };
        Some((Arc::new(bytes), 0))
    };

    let interpreter_settings = InterpreterSettings {
        font_resolver: Arc::new(move |query| match query {
            FontQuery::Standard(s) => select_standard_font(*s),
            FontQuery::Fallback(f) => select_standard_font(f.pick_standard_font()),
        }),
        cmap_resolver: Arc::new(|_| None),
        warning_sink: Arc::new(|_| {}),
        render_annotations: false,
    };

    let cache = RenderCache::new();
    hayro_svg::convert(
        pdf.page(),
        &cache,
        &interpreter_settings,
        &SvgRenderSettings { bg_color: [0, 0, 0, 0] },
    )
}
