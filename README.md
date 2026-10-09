# Typst Editable IDML

Exporta la composición de un documento Typst como IDML para abrirlo y editarlo en InDesign. El texto compatible queda en marcos de texto independientes; los gráficos vectoriales y las imágenes se conservan como recursos colocados.

Proyecto de Jean Paolo Porcel Inquillo, basado en [Typst](https://github.com/typst/typst). Asistencia de desarrollo acreditada: **OpenAI Codex / GPT-6.1 Sol**. La procedencia está descrita en [DEVELOPMENT.md](DEVELOPMENT.md).

## Qué aporta

- Exportación a IDML desde la CLI con `--format idml` o por extensión `.idml`.
- Paquete de entrega con enlaces gráficos relativos, fuentes permitidas y un informe de fuentes omitidas.
- Texto latino horizontal compatible como historias y marcos de texto editables.
- Autoajuste de los marcos al abrirlos para evitar desbordamientos por diferencias de métricas tipográficas.
- Gráficos vectoriales colocados como SVG; imágenes raster empaquetadas junto al IDML.
- Pliego de portada y páginas enfrentadas para documentos editoriales.

## Compilar y exportar

Requiere Rust 1.92 o posterior y las dependencias de compilación de Typst.

```sh
cargo build --release --locked -p typst-cli

# Un archivo IDML
./target/release/typst compile document.typ document.idml --format idml

# Un paquete con el IDML, Links, Document fonts e informes
./target/release/typst compile document.typ document-package --format idml --idml-package

# Seleccionar páginas físicas
./target/release/typst compile document.typ selected-package --format idml --idml-package --pages 16
```

Conserva juntos el archivo IDML y la carpeta `Links`. Las fuentes solo se copian a `Document fonts` cuando sus permisos permiten incluirlas; las demás se enumeran en `Fuentes.txt`.

## Qué significa “texto editable”

Cada fragmento de texto que Typst compone se exporta como una historia y un marco independientes. El archivo conserva la composición fija: editar un texto no vuelve a distribuir el contenido de la página ni mueve los objetos vecinos. Los marcos pueden crecer en alto y ancho al abrir el documento, pero un texto editado puede invadir elementos cercanos.

La primera versión convierte a contornos los fragmentos que no puede representar como texto horizontal compatible, como escrituras complejas y glifos especiales. El texto que forma parte de un SVG importado se conserva como vector dentro de ese gráfico, no como una historia editable de InDesign.

## Límites y validación

El exportador produce IDML, no archivos `.indd`. Abre el IDML en InDesign y guárdalo manualmente como documento de InDesign. Las fuentes no se incrustan dentro del IDML; el paquete incluye archivos solo cuando la licencia lo permite.

El proyecto incluye pruebas automatizadas del paquete, los recursos y la CLI. También se ha abierto y revisado manualmente en InDesign 2026 con documentos editoriales multipágina. Esa comprobación cubre casos representativos y no garantiza fidelidad para todos los documentos, fuentes o versiones de InDesign. Los documentos de trabajo usados en la validación no se distribuyen en este repositorio.

Consulta la [guía técnica del exportador](docs/dev/idml-export.md). Se conservan la [licencia Apache-2.0](LICENSE), los avisos de Typst y las licencias de terceros en [NOTICE](NOTICE). Este fork comunitario no está afiliado al equipo de Typst ni constituye una contribución aceptada por el proyecto upstream.

## English

Typst Editable IDML exports composed Typst documents to IDML for editing in InDesign. Compatible horizontal Latin text runs become independent stories and text frames; vector artwork and raster images are packaged as placed resources. Text frames auto-size when opened, while the surrounding page layout remains fixed.

Build with `cargo build --release --locked -p typst-cli`, then run:

```sh
./target/release/typst compile document.typ document-package --format idml --idml-package
```

The package contains an IDML file, linked artwork, permitted font files and a font report. Keep the IDML and `Links` directory together. Unsupported text is converted to vector outlines. The exporter does not create `.indd` files or reflow paragraphs; open the IDML in InDesign and save it manually as an InDesign document.

Project lead: Jean Paolo Porcel Inquillo. Development assistance credited to OpenAI Codex / GPT-6.1 Sol. See [DEVELOPMENT.md](DEVELOPMENT.md) for provenance.
