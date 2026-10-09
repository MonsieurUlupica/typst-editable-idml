use std::ffi::OsStr;
use std::path::Path;

use chrono::{DateTime, Datelike, Timelike, Utc};
use ecow::eco_format;
use parking_lot::RwLock;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use typst::diag::{
    At, HintedStrResult, HintedString, SourceDiagnostic, SourceResult, StrResult, Warned,
    bail,
};
use typst::foundations::{Datetime, Smart};
use typst::layout::{PageRange, PageRanges};
use typst::model::Document;
use typst::syntax::Span;
use typst_bundle::{Bundle, BundleOptions, VirtualFs};
use typst_html::{HtmlDocument, HtmlFormatOptions, HtmlOptions};
use typst_kit::diagnostics::DiagnosticWorld;
use typst_kit::timer::Timer;
use typst_layout::{Page, PagedDocument};
use typst_pdf::{PdfFormatOptions, PdfOptions, PdfStandards, Timestamp};
use typst_render::{PngFormat, PngFormatOptions, RenderOptions};
use typst_svg::{SvgFormat, SvgFormatOptions, SvgOptions};
use typst_utils::Scalar;

use crate::args::{
    self, CompileArgs, CompileCommand, DepsFormat, DiagnosticFormat, Input, Output,
    OutputFormat, SvgTextOutput, WatchCommand,
};
use crate::deps::write_deps;
use crate::watch::Status;
use crate::world::SystemWorld;
use crate::{set_failed, terminal};

#[cfg(feature = "http-server")]
use typst_kit::server::HttpServer;

/// Execute a compilation command.
pub fn compile(command: &'static CompileCommand) -> HintedStrResult<()> {
    let mut timer = Timer::new_or_placeholder(command.args.timings.clone());
    let mut config = CompileConfig::new(command)?;
    let mut world = SystemWorld::new(
        Some(&command.args.input),
        &command.args.world,
        &command.args.process,
    )
    .map_err(|err| eco_format!("{err}"))?;
    timer.record(&mut world, |world| compile_once(world, &mut config))?
}

/// A preprocessed `CompileCommand`.
pub struct CompileConfig {
    /// Static warnings to emit after compilation.
    pub warnings: Vec<HintedString>,
    /// Whether we are watching.
    pub watching: bool,
    /// Whether to clear the terminal on recompilation (only while watching).
    pub fullscreen: bool,
    /// Path to input Typst file or stdin.
    pub input: Input,
    /// Path to output file (PDF, PNG, SVG, or HTML).
    pub output: Output,
    /// The format of the output file.
    pub output_format: OutputFormat,
    /// Whether to make the serialized document pretty.
    pub pretty: Option<bool>,
    /// How text is represented in SVG output.
    pub svg_text_mode: typst_svg::SvgTextMode,
    /// Whether IDML output should include a sibling-links handoff directory.
    pub idml_package: bool,
    /// Which pages to export.
    pub pages: Option<PageRanges>,
    /// The document's creation date formatted as a UNIX timestamp, with UTC suffix.
    pub creation_timestamp: Option<DateTime<Utc>>,
    /// The format to emit diagnostics in.
    pub diagnostic_format: DiagnosticFormat,
    /// Opens the output file with the default viewer or a specific program after
    /// compilation.
    pub open: Option<Option<String>>,
    /// A list of standards the PDF should conform to.
    pub pdf_standards: Option<PdfStandards>,
    /// Whether to write PDF (accessibility) tags.
    pub tagged: Option<bool>,
    /// A destination to write a list of dependencies to.
    pub deps: Option<Output>,
    /// The format to use for dependencies.
    pub deps_format: DepsFormat,
    /// The PPI (pixels per inch) to use for PNG export.
    pub ppi: Option<f64>,
    /// The export cache for images, used for caching output files in `typst
    /// watch` sessions with images.
    pub export_cache: ExportCache,
    /// Server for `typst watch` to HTML.
    #[cfg(feature = "http-server")]
    pub server: Option<HttpServer>,
}

impl CompileConfig {
    /// Preprocess a `CompileCommand`, producing a compilation config.
    pub fn new(command: &CompileCommand) -> HintedStrResult<Self> {
        Self::new_impl(&command.args, None)
    }

    /// Preprocess a `WatchCommand`, producing a compilation config.
    pub fn watching(command: &WatchCommand) -> HintedStrResult<Self> {
        Self::new_impl(&command.args, Some(command))
    }

    /// The shared implementation of [`CompileConfig::new`] and
    /// [`CompileConfig::watching`].
    fn new_impl(
        args: &CompileArgs,
        watch: Option<&WatchCommand>,
    ) -> HintedStrResult<Self> {
        let mut warnings = Vec::new();
        let input = args.input.clone();

        let output_format = if let Some(specified) = args.format {
            specified
        } else if args.idml_package {
            OutputFormat::Idml
        } else if let Some(Output::Path(output)) = &args.output {
            match output.extension() {
                Some(ext) if ext.eq_ignore_ascii_case("pdf") => OutputFormat::Pdf,
                Some(ext) if ext.eq_ignore_ascii_case("png") => OutputFormat::Png,
                Some(ext) if ext.eq_ignore_ascii_case("svg") => OutputFormat::Svg,
                Some(ext) if ext.eq_ignore_ascii_case("idml") => OutputFormat::Idml,
                Some(ext) if ext.eq_ignore_ascii_case("html") => OutputFormat::Html,
                _ => bail!(
                    "could not infer output format for path {}.\n\
                     consider providing the format manually with `--format/-f`",
                    output.display(),
                ),
            }
        } else {
            OutputFormat::Pdf
        };

        let svg_text_mode = match args.svg_text {
            Some(_) if output_format != OutputFormat::Svg => {
                bail!("`--svg-text` can only be used with SVG output")
            }
            Some(SvgTextOutput::Outline) | None => typst_svg::SvgTextMode::Outline,
            Some(SvgTextOutput::Hybrid) => typst_svg::SvgTextMode::Hybrid,
        };

        if args.idml_package && output_format != OutputFormat::Idml {
            bail!("`--idml-package` can only be used with IDML output");
        }
        if args.idml_package && matches!(&args.output, Some(Output::Stdout)) {
            bail!("`--idml-package` requires an output directory, not stdout");
        }

        let output = args.output.clone().unwrap_or_else(|| {
            let Input::Path(path) = &input else {
                panic!("output must be specified when input is from stdin, as guarded by the CLI");
            };
            Output::Path(path.with_extension(if args.idml_package {
                "idml-package"
            } else {
                match output_format {
                    OutputFormat::Pdf => "pdf",
                    OutputFormat::Png => "png",
                    OutputFormat::Svg => "svg",
                    OutputFormat::Idml => "idml",
                    OutputFormat::Html => "html",
                    OutputFormat::Bundle => "",
                }
            }))
        });

        let pages = args.pages.as_ref().map(|export_ranges| {
            PageRanges::new(
                export_ranges
                    .iter()
                    .map(|r| PageRange::new(*r.0.start(), *r.0.end()))
                    .collect(),
            )
        });

        if args.no_pdf_tags {
            warnings.push(
                "`--no-pdf-tags` is deprecated, use `--pdf-tagged=false` instead".into(),
            );
        }

        let tagged = if let Some(tagged) = args.pdf_tagged {
            if tagged && pages.is_some() {
                bail!("cannot enable PDF tags when exporting a page range");
            }
            Some(tagged)
        } else {
            if output_format == OutputFormat::Pdf && pages.is_some() && !args.no_pdf_tags
            {
                warnings.push(
                    HintedString::from("using `--pages` implies `--pdf-tagged=false`")
                        .with_hints([
                            "the resulting PDF will be inaccessible".into(),
                            "add `--pdf-tagged=false` to silence this warning".into(),
                        ]),
                );
            }

            (args.no_pdf_tags || pages.is_some()).then_some(false)
        };

        if tagged == Some(false) {
            const ACCESSIBLE: &[(args::PdfStandard, &str)] = &[
                (args::PdfStandard::A_1a, "PDF/A-1a"),
                (args::PdfStandard::A_2a, "PDF/A-2a"),
                (args::PdfStandard::A_3a, "PDF/A-3a"),
                (args::PdfStandard::UA_1, "PDF/UA-1"),
            ];

            for (standard, name) in ACCESSIBLE {
                if args.pdf_standard.contains(standard) {
                    if args.no_pdf_tags {
                        bail!("cannot disable PDF tags when exporting a {name} document");
                    } else {
                        let mut err = HintedString::from(eco_format!(
                            "cannot disable PDF tags when exporting a {name} document",
                        ));
                        if args.pages.is_some() {
                            err.hint("using --pages implies --pdf-tagged=false");
                        }
                        return Err(err);
                    }
                }
            }
        }

        let pdf_standards = (!args.pdf_standard.is_empty())
            .then(|| PdfStandards::new(args.pdf_standard.iter().copied()))
            .transpose()?;

        #[cfg(feature = "http-server")]
        let server = if let Some(command) = watch
            && !command.server.no_serve
            && matches!(output_format, OutputFormat::Html | OutputFormat::Bundle)
        {
            Some(HttpServer::new(
                &eco_format!("{input}"),
                command.server.port,
                !command.server.no_reload,
            )?)
        } else {
            None
        };

        let fullscreen = watch.map(|command| !command.no_fullscreen).unwrap_or(true);

        let mut deps = args.deps.clone();
        let mut deps_format = args.deps_format;

        if let Some(path) = &args.make_deps
            && deps.is_none()
        {
            deps = Some(Output::Path(path.clone()));
            deps_format = DepsFormat::Make;
            warnings.push(
                "--make-deps is deprecated, use --deps and --deps-format instead".into(),
            );
        }

        match (&output, &deps, watch) {
            (Output::Stdout, _, Some(_)) => {
                bail!("cannot write document to stdout in watch mode");
            }
            (_, Some(Output::Stdout), Some(_)) => {
                bail!("cannot write dependencies to stdout in watch mode")
            }
            (Output::Stdout, Some(Output::Stdout), _) => {
                bail!("cannot write both output and dependencies to stdout")
            }
            _ => {}
        }

        Ok(Self {
            warnings,
            watching: watch.is_some(),
            input,
            output,
            output_format,
            pretty: args.pretty,
            svg_text_mode,
            idml_package: args.idml_package,
            pages,
            pdf_standards,
            tagged,
            creation_timestamp: args
                .world
                .creation_timestamp
                .map(|time| {
                    chrono::DateTime::from_timestamp(time, 0)
                        .ok_or("creation timestamp is out of range")
                })
                .transpose()?,
            ppi: args.ppi,
            diagnostic_format: args.process.diagnostic_format,
            open: args.open.clone(),
            export_cache: ExportCache::new(),
            deps,
            deps_format,
            #[cfg(feature = "http-server")]
            server,
            fullscreen,
        })
    }
}

/// Compile a single time.
///
/// Returns whether it compiled without errors.
#[typst_macros::time(name = "compile once")]
pub fn compile_once(
    world: &mut SystemWorld,
    config: &mut CompileConfig,
) -> HintedStrResult<()> {
    let start = std::time::Instant::now();
    if config.watching {
        Status::Compiling.print(config).unwrap();
    }

    let Warned { output, mut warnings } = compile_and_export(world, config);

    // Add static warnings (for deprecated CLI flags and such).
    for warning in &config.warnings {
        warnings.push(warning.clone().into_warning_at(Span::detached()));
    }

    match &output {
        // Print success message and possibly warnings.
        Ok(_) => {
            let duration = start.elapsed();
            if config.watching {
                if warnings.is_empty() {
                    Status::Success(duration).print(config).unwrap();
                } else {
                    Status::PartialSuccess(duration).print(config).unwrap();
                }
            }

            print_diagnostics(world, &[], &warnings, config.diagnostic_format)
                .map_err(|err| eco_format!("failed to print diagnostics ({err})"))?;

            open_output(config)?;
        }

        // Print failure message and diagnostics.
        Err(errors) => {
            set_failed();

            if config.watching {
                Status::Error.print(config).unwrap();
            }

            print_diagnostics(world, errors, &warnings, config.diagnostic_format)
                .map_err(|err| eco_format!("failed to print diagnostics ({err})"))?;
        }
    }

    if let Some(dest) = &config.deps {
        write_deps(world, dest, config.deps_format, output.as_deref().ok())
            .map_err(|err| eco_format!("failed to create dependency file ({err})"))?;
    }

    Ok(())
}

/// Compile and then export the document.
fn compile_and_export(
    world: &mut SystemWorld,
    config: &mut CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    match config.output_format {
        OutputFormat::Pdf
        | OutputFormat::Png
        | OutputFormat::Svg
        | OutputFormat::Idml => typst::compile::<PagedDocument>(world)
            .and_then(|document| export_paged(world, &document, config)),
        OutputFormat::Html => {
            let Warned { output, warnings } = typst::compile::<HtmlDocument>(world);
            let result = output.and_then(|document| export_html(&document, config));
            Warned {
                output: result.map(|()| vec![config.output.clone()]),
                warnings,
            }
        }
        OutputFormat::Bundle => typst::compile::<Bundle>(world)
            .and_then(|bundle| export_bundle(bundle, config)),
    }
}

/// Export to HTML.
fn export_html(document: &HtmlDocument, config: &CompileConfig) -> SourceResult<()> {
    let options = html_options(config);
    let html = typst_html::html(document, &options)?;
    let result = config.output.write(html.as_bytes());

    #[cfg(feature = "http-server")]
    if let Some(server) = &config.server {
        server.set_html(html);
    }

    result
        .map_err(|err| eco_format!("failed to write HTML file ({err})"))
        .at(Span::detached())
}

/// Export to a paged target format.
fn export_paged(
    world: &SystemWorld,
    document: &PagedDocument,
    config: &CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    match config.output_format {
        OutputFormat::Pdf => {
            export_pdf(document, config).and_then(|()| Ok(vec![config.output.clone()]))
        }
        OutputFormat::Png => export_image(document, config, ImageExportFormat::Png)
            .at(Span::detached())
            .into(),
        OutputFormat::Svg => export_svg(document, config),
        OutputFormat::Idml => export_idml(world, document, config),
        OutputFormat::Html | OutputFormat::Bundle => unreachable!(),
    }
}

/// Export all selected pages into one IDML package.
fn export_idml(
    world: &SystemWorld,
    document: &PagedDocument,
    config: &CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    let options = typst_idml::IdmlOptions { pages: config.pages.clone() };
    if !config.idml_package {
        return typst_idml::idml(document, &options).and_then(|bytes| {
            config
                .output
                .write(&bytes)
                .map_err(|err| eco_format!("failed to write IDML file ({err})"))
                .at(Span::detached())?;
            Ok(vec![config.output.clone()])
        });
    }

    let Output::Path(package_root) = &config.output else {
        return idml_export_error("`--idml-package` requires an output directory");
    };
    let Input::Path(input) = &config.input else {
        return idml_export_error("`--idml-package` requires a file input");
    };
    let Some(input_name) = input.file_name() else {
        return idml_export_error("could not determine the input file name");
    };
    let document_name = Path::new(input_name).with_extension("idml");
    let links_dir = package_root.join("Links");
    if let Err(err) = std::fs::create_dir_all(&links_dir) {
        return idml_export_error(&eco_format!(
            "failed to create package Links directory ({err})"
        ));
    }

    typst_idml::idml_package_with_world(document, &options, world).and_then(|package| {
        let document_path = package_root.join(&document_name);
        std::fs::create_dir_all(package_root)
            .map_err(|err| eco_format!("failed to create IDML package directory ({err})"))
            .at(Span::detached())?;
        std::fs::write(&document_path, &package.document)
            .map_err(|err| eco_format!("failed to write IDML document ({err})"))
            .at(Span::detached())?;

        let mut outputs = vec![Output::Path(document_path)];
        for (name, contents) in package.links {
            let path = links_dir.join(name);
            std::fs::write(&path, contents)
                .map_err(|err| eco_format!("failed to write linked asset ({err})"))
                .at(Span::detached())?;
            outputs.push(Output::Path(path));
        }

        let document_fonts_dir = package_root.join("Document fonts");
        std::fs::create_dir_all(&document_fonts_dir)
            .map_err(|err| eco_format!("failed to create Document fonts directory ({err})"))
            .at(Span::detached())?;
        let mut report = String::from(
            "Paquete IDML\n\nAbre el archivo IDML de esta carpeta y conserva las carpetas Links y Document fonts junto a él. InDesign carga temporalmente las fuentes de Document fonts al abrir el documento.\n\nFuentes usadas por texto Typst\n\n",
        );
        if package.fonts.is_empty() {
            report.push_str("No se detectaron fuentes en los marcos de texto exportados.\n");
        }
        for font in package.fonts {
            report.push_str("- ");
            report.push_str(&font.faces.join(", "));
            if !font.sources.is_empty() {
                report.push_str(" (SVG: ");
                report.push_str(
                    &font
                        .sources
                        .iter()
                        .map(|source| source.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                report.push(')');
            }
            match (font.filename, font.data) {
                (Some(name), Some(data)) => {
                    let path = document_fonts_dir.join(name);
                    std::fs::write(&path, data)
                        .map_err(|err| eco_format!("failed to write packaged font ({err})"))
                        .at(Span::detached())?;
                    report.push_str(": copiada a Document fonts/\n");
                    outputs.push(Output::Path(path));
                }
                (_, _) => {
                    report.push_str(": no copiada");
                    if let Some(reason) = font.omitted_reason {
                        report.push_str(" (");
                        report.push_str(&reason);
                        report.push(')');
                    }
                    report.push('\n');
                }
            }
        }
        let report_path = package_root.join("Fuentes.txt");
        std::fs::write(&report_path, report)
            .map_err(|err| eco_format!("failed to write font report ({err})"))
            .at(Span::detached())?;
        outputs.push(Output::Path(report_path));

        let instructions_path = package_root.join("LEEME.txt");
        let instructions = format!(
            "Paquete IDML generado por Typst.\n\nDocumento: {}\nGráficos vinculados: Links/\nFuentes permitidas: Document fonts/\nInforme de fuentes: Fuentes.txt\n\nMantén el documento IDML y las carpetas Links y Document fonts dentro de esta carpeta.\n",
            document_name.display(),
        );
        std::fs::write(&instructions_path, instructions)
            .map_err(|err| eco_format!("failed to write package instructions ({err})"))
            .at(Span::detached())?;
        outputs.push(Output::Path(instructions_path));
        Ok(outputs)
    })
}

fn idml_export_error(message: &str) -> Warned<SourceResult<Vec<Output>>> {
    let mut errors = ecow::EcoVec::new();
    errors.push(SourceDiagnostic::error(Span::detached(), message));
    Warned { output: Err(errors), warnings: ecow::EcoVec::new() }
}

/// Export to a PDF.
fn export_pdf(
    document: &PagedDocument,
    config: &CompileConfig,
) -> Warned<SourceResult<()>> {
    let options = pdf_options(config);
    typst_pdf::pdf(document, &options).and_then(|buffer| {
        config
            .output
            .write(&buffer)
            .map_err(|err| eco_format!("failed to write PDF file ({err})"))
            .at(Span::detached())?;
        Ok(())
    })
}

/// Export to a bundle, a collection of files in a directory.
fn export_bundle(
    bundle: Bundle,
    config: &CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    let options = BundleOptions {
        html: html_options(config),
        pdf: pdf_options(config),
        png: png_options(config),
        svg: svg_options(config),
    };

    typst_bundle::export(&bundle, &options).and_then(|fs| {
        let root = match &config.output {
            Output::Path(path) => path,
            Output::Stdout => {
                bail!(Span::detached(), "cannot write bundle to standard output")
            }
        };

        let outputs = write_virtual_fs(root, &fs).at(Span::detached())?;

        #[cfg(feature = "http-server")]
        if let Some(server) = &config.server {
            server.set_bundle(bundle, fs);
        }

        Ok(outputs)
    })
}

/// Writes a bundle's files to disk.
fn write_virtual_fs(root: &Path, fs: &VirtualFs) -> StrResult<Vec<Output>> {
    std::fs::create_dir_all(root)
        .map_err(|err| eco_format!("failed to create output directory ({err})"))?;

    fs.par_iter()
        .map(|(path, data)| {
            let realized = path
                .realize(root)
                .map_err(|err| eco_format!("failed to realize path ({err})"))?;

            if let Some(parent) = realized.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| eco_format!("failed to create directory ({err})"))?;
            }

            std::fs::write(&realized, data)
                .map_err(|err| eco_format!("failed to write file ({err})"))?;
            Ok(Output::Path(realized))
        })
        .collect()
}

/// Convert [`chrono::DateTime`] to [`Datetime`]
fn convert_datetime<Tz: chrono::TimeZone>(
    date_time: chrono::DateTime<Tz>,
) -> Option<Datetime> {
    Datetime::from_ymd_hms(
        date_time.year(),
        date_time.month().try_into().ok()?,
        date_time.day().try_into().ok()?,
        date_time.hour().try_into().ok()?,
        date_time.minute().try_into().ok()?,
        date_time.second().try_into().ok()?,
    )
}

/// An image format to export in.
#[derive(Copy, Clone)]
enum ImageExportFormat {
    Png,
}

/// Export to one or multiple images.
fn export_image(
    document: &PagedDocument,
    config: &CompileConfig,
    fmt: ImageExportFormat,
) -> StrResult<Vec<Output>> {
    match fmt {
        ImageExportFormat::Png => {
            let options =
                png_options(config).resolve(document.options().get::<PngFormat>());
            export_image_pages(config, document, |page, output, _page_number| {
                let pixmap = typst_render::render(page, &options);
                let buf = pixmap
                    .encode_png()
                    .map_err(|err| eco_format!("failed to encode PNG file ({err})"))?;
                output
                    .write(&buf)
                    .map_err(|err| eco_format!("failed to write PNG file ({err})"))?;
                Ok(Vec::new())
            })
            .map(|(outputs, _warnings)| outputs)
        }
    }
}

/// Export SVG pages, propagating hybrid SVG fallback warnings.
fn export_svg(
    document: &PagedDocument,
    config: &CompileConfig,
) -> Warned<SourceResult<Vec<Output>>> {
    let options = svg_options(config).resolve(document.options().get::<SvgFormat>());
    match export_image_pages(config, document, |page, output, page_number| {
        let result = typst_svg::try_svg_with_text_mode(
            page,
            &options,
            config.svg_text_mode,
            page_number,
        )?;
        output
            .write(result.svg.as_bytes())
            .map_err(|err| eco_format!("failed to write SVG file ({err})"))?;
        Ok(result
            .warnings
            .into_iter()
            .map(|warning| warning.to_string())
            .collect())
    }) {
        Ok((outputs, warnings)) => Warned {
            output: Ok(outputs),
            warnings: warnings
                .iter()
                .map(|warning| typst::diag::warning!(Span::detached(), "{}", warning))
                .collect(),
        },
        Err(error) => Warned {
            output: Err(vec![typst::diag::error!(Span::detached(), "{}", error)].into()),
            warnings: Vec::new().into(),
        },
    }
}

/// Exports the document's pages as separate images, each exported with a
/// format-specific `export_image_page` function.
fn export_image_pages<F>(
    config: &CompileConfig,
    document: &PagedDocument,
    export_image_page: F,
) -> StrResult<(Vec<Output>, Vec<String>)>
where
    F: Fn(&Page, &Output, usize) -> StrResult<Vec<String>>,
    F: Send + Sync,
{
    // Determine whether we have indexable templates in output
    let can_handle_multiple = match config.output {
        Output::Stdout => false,
        Output::Path(ref output) => {
            output_template::has_indexable_template(output.to_str().unwrap_or_default())
        }
    };

    let exported_pages = document
        .pages()
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            config.pages.as_ref().is_none_or(|exported_page_ranges| {
                exported_page_ranges.includes_page_index(*i)
            })
        })
        .collect::<Vec<_>>();

    if !can_handle_multiple && exported_pages.len() > 1 {
        let err = match config.output {
            Output::Stdout => "to stdout",
            Output::Path(_) => {
                "without a page number template ({p}, {0p}) in the output path"
            }
        };
        bail!("cannot export multiple images {err}");
    }

    // The results are collected in a `Vec<()>` which does not allocate.
    exported_pages
        .par_iter()
        .map(|(i, page)| {
            // Use output with converted path.
            let output = match &config.output {
                Output::Path(path) => {
                    let storage;
                    let path = if can_handle_multiple {
                        storage = output_template::format(
                            path.to_str().unwrap_or_default(),
                            i + 1,
                            document.pages().len(),
                        );
                        Path::new(&storage)
                    } else {
                        path
                    };

                    // If we are not watching, don't use the cache.
                    // If the frame is in the cache, skip it.
                    // If the file does not exist, always create it.
                    if config.watching
                        && config.export_cache.is_cached(*i, page)
                        && path.exists()
                    {
                        return Ok((Output::Path(path.to_owned()), Vec::new()));
                    }

                    Output::Path(path.to_owned())
                }
                Output::Stdout => Output::Stdout,
            };

            let warnings = export_image_page(page, &output, i + 1)?;

            Ok((output, warnings))
        })
        .collect::<StrResult<Vec<_>>>()
        .map(|results| {
            let mut outputs = Vec::with_capacity(results.len());
            let mut warnings = Vec::new();
            for (output, page_warnings) in results {
                outputs.push(output);
                warnings.extend(page_warnings);
            }
            (outputs, warnings)
        })
}

mod output_template {
    const INDEXABLE: [&str; 3] = ["{p}", "{0p}", "{n}"];

    pub fn has_indexable_template(output: &str) -> bool {
        INDEXABLE.iter().any(|template| output.contains(template))
    }

    pub fn format(output: &str, this_page: usize, total_pages: usize) -> String {
        // Find the base 10 width of number `i`
        fn width(i: usize) -> usize {
            1 + i.checked_ilog10().unwrap_or(0) as usize
        }

        let other_templates = ["{t}"];
        INDEXABLE.iter().chain(other_templates.iter()).fold(
            output.to_string(),
            |out, template| {
                let replacement = match *template {
                    "{p}" => format!("{this_page}"),
                    "{0p}" | "{n}" => format!("{:01$}", this_page, width(total_pages)),
                    "{t}" => format!("{total_pages}"),
                    _ => unreachable!("unhandled template placeholder {template}"),
                };
                out.replace(template, replacement.as_str())
            },
        )
    }
}

/// Creates options for HTML export.
fn html_options(config: &CompileConfig) -> HtmlOptions {
    HtmlOptions {
        format: HtmlFormatOptions { pretty: config.pretty },
    }
}

/// Creates options for PDF export.
fn pdf_options(config: &CompileConfig) -> PdfOptions {
    // If the timestamp is provided through the CLI, use UTC suffix,
    // else, use the current local time and timezone.
    let timestamp = match config.creation_timestamp {
        Some(timestamp) => convert_datetime(timestamp).map(Timestamp::new_utc),
        None => {
            let local_datetime = chrono::Local::now();
            convert_datetime(local_datetime).and_then(|datetime| {
                Timestamp::new_local(
                    datetime,
                    local_datetime.offset().local_minus_utc() / 60,
                )
            })
        }
    };

    PdfOptions {
        ident: Smart::Auto,
        creator: Smart::Auto,
        timestamp,
        format: PdfFormatOptions {
            pages: config.pages.clone().map(Some),
            standard: config.pdf_standards.clone(),
            tagged: config.tagged.map(Smart::Custom),
            pretty: config.pretty,
        },
    }
}

/// Creates options for SVG export.
fn svg_options(config: &CompileConfig) -> SvgOptions {
    SvgOptions {
        render_bleed: false,
        format: SvgFormatOptions { pretty: config.pretty },
    }
}

/// Creates options for PNG export.
fn png_options(config: &CompileConfig) -> RenderOptions {
    RenderOptions {
        render_bleed: false,
        format: PngFormatOptions { ppi: config.ppi.map(Scalar::new) },
    }
}

/// Caches exported files so that we can avoid re-exporting them if they haven't
/// changed.
///
/// This is done by having a list of size `files.len()` that contains the hashes
/// of the last rendered frame in each file. If a new frame is inserted, this
/// will invalidate the rest of the cache, this is deliberate as to decrease the
/// complexity and memory usage of such a cache.
pub struct ExportCache {
    /// The hashes of last compilation's frames.
    pub cache: RwLock<Vec<u128>>,
}

impl ExportCache {
    /// Creates a new export cache.
    pub fn new() -> Self {
        Self { cache: RwLock::new(Vec::with_capacity(32)) }
    }

    /// Returns true if the entry is cached and appends the new hash to the
    /// cache (for the next compilation).
    pub fn is_cached(&self, i: usize, page: &Page) -> bool {
        let hash = typst::utils::hash128(page);

        let mut cache = self.cache.upgradable_read();
        if i >= cache.len() {
            cache.with_upgraded(|cache| cache.push(hash));
            return false;
        }

        cache.with_upgraded(|cache| std::mem::replace(&mut cache[i], hash) == hash)
    }
}

/// Opens the output if desired.
fn open_output(config: &mut CompileConfig) -> StrResult<()> {
    let Some(viewer) = config.open.take() else { return Ok(()) };

    #[cfg(feature = "http-server")]
    if let Some(server) = &config.server {
        let url = format!("http://{}", server.addr());
        return open_path(OsStr::new(&url), viewer.as_deref());
    }

    // Can't open stdout.
    let Output::Path(path) = &config.output else { return Ok(()) };
    let package_document;
    let path = if config.idml_package {
        let Input::Path(input) = &config.input else { return Ok(()) };
        let Some(name) = input.file_name() else { return Ok(()) };
        package_document = path.join(Path::new(name).with_extension("idml"));
        &package_document
    } else {
        path
    };

    // Some resource openers require the path to be canonicalized.
    let path = path
        .canonicalize()
        .map_err(|err| eco_format!("failed to canonicalize path ({err})"))?;

    open_path(path.as_os_str(), viewer.as_deref())
}

/// Opens the given file using:
///
/// - The default file viewer if `app` is `None`.
/// - The given viewer provided by `app` if it is `Some`.
fn open_path(path: &OsStr, viewer: Option<&str>) -> StrResult<()> {
    if let Some(viewer) = viewer {
        open::with_detached(path, viewer)
            .map_err(|err| eco_format!("failed to open file with {viewer} ({err})"))
    } else {
        open::that_detached(path).map_err(|err| {
            let openers = open::commands(path)
                .iter()
                .map(|command| command.get_program().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ");
            eco_format!(
                "failed to open file with any of these resource openers: {openers} \
                 ({err})",
            )
        })
    }
}

/// Print diagnostic messages to the terminal.
pub fn print_diagnostics(
    world: &dyn DiagnosticWorld,
    errors: &[SourceDiagnostic],
    warnings: &[SourceDiagnostic],
    format: DiagnosticFormat,
) -> Result<(), codespan_reporting::files::Error> {
    typst_kit::diagnostics::emit(
        &mut terminal::out(),
        world,
        errors.iter().chain(warnings),
        match format {
            DiagnosticFormat::Human => typst_kit::diagnostics::DiagnosticFormat::Human,
            DiagnosticFormat::Short => typst_kit::diagnostics::DiagnosticFormat::Short,
        },
    )
}

impl From<args::PdfStandard> for typst_pdf::PdfStandard {
    fn from(standard: args::PdfStandard) -> Self {
        match standard {
            args::PdfStandard::V_1_4 => typst_pdf::PdfStandard::V_1_4,
            args::PdfStandard::V_1_5 => typst_pdf::PdfStandard::V_1_5,
            args::PdfStandard::V_1_6 => typst_pdf::PdfStandard::V_1_6,
            args::PdfStandard::V_1_7 => typst_pdf::PdfStandard::V_1_7,
            args::PdfStandard::V_2_0 => typst_pdf::PdfStandard::V_2_0,
            args::PdfStandard::A_1b => typst_pdf::PdfStandard::A_1b,
            args::PdfStandard::A_1a => typst_pdf::PdfStandard::A_1a,
            args::PdfStandard::A_2b => typst_pdf::PdfStandard::A_2b,
            args::PdfStandard::A_2u => typst_pdf::PdfStandard::A_2u,
            args::PdfStandard::A_2a => typst_pdf::PdfStandard::A_2a,
            args::PdfStandard::A_3b => typst_pdf::PdfStandard::A_3b,
            args::PdfStandard::A_3u => typst_pdf::PdfStandard::A_3u,
            args::PdfStandard::A_3a => typst_pdf::PdfStandard::A_3a,
            args::PdfStandard::A_4 => typst_pdf::PdfStandard::A_4,
            args::PdfStandard::A_4f => typst_pdf::PdfStandard::A_4f,
            args::PdfStandard::A_4e => typst_pdf::PdfStandard::A_4e,
            args::PdfStandard::UA_1 => typst_pdf::PdfStandard::UA_1,
        }
    }
}
