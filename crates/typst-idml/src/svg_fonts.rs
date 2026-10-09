//! Font discovery and family normalization for linked SVG assets.

use std::collections::BTreeSet;

use quick_xml::Decoder;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use typst_library::text::{FontStretch, FontStyle, FontVariant, FontWeight};

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct SvgFontUse {
    pub family: String,
    pub variant: FontVariant,
}

#[derive(Debug, Clone)]
struct TextStyle {
    family: Option<String>,
    weight: u16,
    style: FontStyle,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: None,
            weight: 400,
            style: FontStyle::Normal,
        }
    }
}

/// Removes generic fallback names from SVG font-family lists and discovers
/// the concrete font faces used by text in the asset.
pub(crate) fn normalize_and_collect(
    svg: &[u8],
) -> Result<(Vec<u8>, Vec<SvgFontUse>), String> {
    let mut reader = Reader::from_reader(svg);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::with_capacity(svg.len()));
    let mut buffer = Vec::new();
    let mut stack = Vec::<TextStyle>::new();
    let mut stylesheets = 0usize;
    let mut uses = BTreeSet::new();

    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("could not parse linked SVG XML ({error})"))?
        {
            Event::Start(element) => {
                let local_name = element.local_name().as_ref().to_vec();
                let inherited = stack.last().cloned().unwrap_or_default();
                let (element, computed) =
                    rewrite_start(&element, reader.decoder(), inherited)?;
                if local_name.as_slice() == b"style" {
                    stylesheets += 1;
                }
                stack.push(computed);
                writer.write_event(Event::Start(element)).map_err(|error| {
                    format!("could not rewrite linked SVG XML ({error})")
                })?;
            }
            Event::Empty(element) => {
                let inherited = stack.last().cloned().unwrap_or_default();
                let (element, _) = rewrite_start(&element, reader.decoder(), inherited)?;
                writer.write_event(Event::Empty(element)).map_err(|error| {
                    format!("could not rewrite linked SVG XML ({error})")
                })?;
            }
            Event::End(element) => {
                if element.local_name().as_ref() == b"style" {
                    stylesheets = stylesheets.saturating_sub(1);
                }
                stack.pop();
                writer.write_event(Event::End(element.into_owned())).map_err(
                    |error| format!("could not rewrite linked SVG XML ({error})"),
                )?;
            }
            Event::Text(text) if stylesheets > 0 => {
                let decoded = text.decode().map_err(|error| {
                    format!("could not decode linked SVG CSS ({error})")
                })?;
                let css = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    format!("could not decode linked SVG CSS ({error})")
                })?;
                let normalized = normalize_stylesheet(&css, &mut uses);
                writer.write_event(Event::Text(BytesText::new(&normalized))).map_err(
                    |error| format!("could not rewrite linked SVG CSS ({error})"),
                )?;
            }
            Event::Text(text) => {
                let decoded = text.xml_content().map_err(|error| {
                    format!("could not decode linked SVG text ({error})")
                })?;
                if !decoded.trim().is_empty()
                    && let Some(style) = stack.last()
                {
                    record_use(style, &mut uses);
                }
                writer.write_event(Event::Text(text.into_owned())).map_err(|error| {
                    format!("could not rewrite linked SVG XML ({error})")
                })?;
            }
            Event::Eof => break,
            event => writer
                .write_event(event.into_owned())
                .map_err(|error| format!("could not rewrite linked SVG XML ({error})"))?,
        }
        buffer.clear();
    }

    Ok((writer.into_inner(), uses.into_iter().collect()))
}

fn rewrite_start(
    original: &BytesStart<'_>,
    decoder: Decoder,
    mut computed: TextStyle,
) -> Result<(BytesStart<'static>, TextStyle), String> {
    let name = std::str::from_utf8(original.name().as_ref())
        .map_err(|error| format!("linked SVG has an invalid element name ({error})"))?
        .to_owned();
    let mut attributes = Vec::new();
    for attribute in original.attributes() {
        let attribute = attribute
            .map_err(|error| format!("linked SVG has an invalid attribute ({error})"))?;
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|error| {
                format!("linked SVG has an invalid attribute name ({error})")
            })?
            .to_owned();
        let value = attribute
            .decode_and_unescape_value(decoder)
            .map_err(|error| format!("could not decode linked SVG attribute ({error})"))?
            .into_owned();
        attributes.push((key, value));
    }

    // Presentation attributes are inherited by child SVG elements. The
    // inline `style` declaration is applied afterward because it has priority.
    for (key, value) in &attributes {
        match key.as_str() {
            "font-family" => apply_family(&mut computed, value),
            "font-weight" => computed.weight = parse_weight(value, computed.weight),
            "font-style" => computed.style = parse_style(value, computed.style),
            _ => {}
        }
    }

    let mut normalized = BytesStart::new(name);
    for (key, value) in &mut attributes {
        match key.as_str() {
            "font-family" => {
                if let Some(family) = normalized_family(value) {
                    *value = family.clone();
                }
            }
            "style" => *value = normalize_inline_style(value, &mut computed),
            _ => {}
        }
        normalized.push_attribute((key.as_str(), value.as_str()));
    }

    Ok((normalized, computed))
}

fn normalize_inline_style(style: &str, computed: &mut TextStyle) -> String {
    let mut declarations = Vec::new();
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            if !declaration.trim().is_empty() {
                declarations.push(declaration.trim().to_owned());
            }
            continue;
        };

        let property = property.trim();
        let mut value = value.trim().to_owned();
        match property.to_ascii_lowercase().as_str() {
            "font-family" => {
                if let Some(family) = normalized_family(&value) {
                    value = family.clone();
                    computed.family = Some(family);
                }
            }
            "font-weight" => computed.weight = parse_weight(&value, computed.weight),
            "font-style" => computed.style = parse_style(&value, computed.style),
            _ => {}
        }
        declarations.push(format!("{property}: {value}"));
    }
    declarations.join(";")
}

fn normalize_stylesheet(css: &str, uses: &mut BTreeSet<SvgFontUse>) -> String {
    let mut output = String::with_capacity(css.len());
    let mut remainder = css;

    while let Some(open) = remainder.find('{') {
        let Some(close) = remainder[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close;
        let selector = remainder[..open].trim();
        let declarations = &remainder[open + 1..close];
        let mut computed = TextStyle::default();
        let normalized = normalize_inline_style(declarations, &mut computed);
        if !selector.starts_with("@font-face") {
            record_use(&computed, uses);
        }
        output.push_str(&remainder[..open + 1]);
        output.push_str(&normalized);
        output.push('}');
        remainder = &remainder[close + 1..];
    }
    output.push_str(remainder);
    output
}

fn normalized_family(value: &str) -> Option<String> {
    let candidates = split_families(value);
    let family = candidates
        .iter()
        .find(|candidate| !is_generic_family(candidate))
        .or_else(|| candidates.first())?;
    Some(family.clone())
}

fn split_families(value: &str) -> Vec<String> {
    let mut families = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            current.push(character);
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            ',' => {
                let family = current.trim();
                if !family.is_empty() {
                    families.push(family.to_owned());
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let family = current.trim();
    if !family.is_empty() {
        families.push(family.to_owned());
    }
    families
}

fn is_generic_family(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "math"
            | "emoji"
            | "fangsong"
    )
}

fn parse_weight(value: &str, inherited: u16) -> u16 {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => 400,
        "bold" => 700,
        "bolder" => match inherited {
            100..=400 => 700,
            401..=700 => 900,
            _ => 900,
        },
        "lighter" => match inherited {
            700..=900 => 400,
            _ => 300,
        },
        value => value
            .parse::<u16>()
            .map_or(inherited, |weight| weight.clamp(100, 900)),
    }
}

fn parse_style(value: &str, inherited: FontStyle) -> FontStyle {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => FontStyle::Normal,
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => inherited,
    }
}

fn apply_family(style: &mut TextStyle, value: &str) {
    if value.trim().eq_ignore_ascii_case("inherit") {
        return;
    }
    if value.trim().eq_ignore_ascii_case("initial") {
        style.family = None;
    } else if let Some(family) = normalized_family(value) {
        style.family = Some(family);
    }
}

fn record_use(style: &TextStyle, uses: &mut BTreeSet<SvgFontUse>) {
    let Some(family) = style.family.as_ref() else { return };
    uses.insert(SvgFontUse {
        family: family.clone(),
        variant: FontVariant::new(
            style.style,
            FontWeight::from_number(style.weight),
            FontStretch::NORMAL,
        ),
    });
}
