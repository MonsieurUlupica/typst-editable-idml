# Typst Editable SVG

Exportación SVG híbrida con texto editable y gráficos vectoriales integrados.
Proyecto de **Jean Porcel**, basado en el compilador de [Typst](https://github.com/typst/typst).
Asistencia de desarrollo acreditada: **OpenAI Codex / GPT-6.1 Sol**.
Registro de procedencia: [DEVELOPMENT.md](DEVELOPMENT.md).

## Qué aporta

- Texto latino compatible como objetos SVG `<text>` independientes.
- Contornos para escrituras y glifos no admitidos.
- SVG importados integrados como grupos vectoriales con identificadores únicos.
- PNG y JPEG incrustados, sin enlaces a sus archivos originales.
- Respaldo PNG a 300 ppi con aviso cuando un SVG no puede integrarse de forma segura.
- Un SVG por página, con `outline` como modo predeterminado.

Fork independiente mantenido por Jean Porcel. Los archivos exportados son SVG autocontenidos.
La versión `v0.1.0` corresponde a la extensión experimental; la base de Typst es `0.15.1`.

## Compilar

Requiere Rust 1.92 o posterior y el enlazador de C/C++ de la plataforma.
Desde la raíz del repositorio:

```sh
cargo build --release --locked -p typst-cli
./target/release/typst compile document.typ 'page-{0p}.svg' --format svg --svg-text hybrid
```

En Windows el ejecutable es `target/release/typst.exe`.
Para contornos: `--svg-text outline`. Para seleccionar una página: `--pages 1`.

## Ejemplos públicos

```sh
bash scripts/export-examples.sh
```

Produce demostraciones originales en `examples/output/`.
Abre un SVG en un editor SVG, selecciona texto, cambia sus caracteres y guarda el SVG.
Instala las fuentes indicadas en un editor SVG: el SVG no las incrusta.

## Límites

Cada fragmento compuesto por Typst es un objeto de texto; no se reconstruyen párrafos con reflujo.
El editor SVG vuelve a componer los glifos y puede cambiar el espaciado. Escrituras complejas,
emoji, fuentes variables y otros casos no admitidos se convierten a contornos.
El texto interno de un SVG importado puede editarse si su fuente lo define como `<text>`.
La fidelidad de filtros y recortes depende del importador del editor SVG.

Hay pruebas automatizadas del exportador y la CLI. Jean Porcel ha revisado resultados en
SVG editor; esto no certifica compatibilidad universal con todas sus versiones ni todos los SVG.
Resultados reproducibles: [informe de validación](docs/validation-report.md).
Consulta [la guía técnica](docs/dev/editable-svg.md) y [la prueba manual](docs/svg-validation.md).

## Créditos y licencia

Dirección, requisitos, revisión y pruebas editoriales: **Jean Porcel**.
Compilador y renderizado base: **The Typst Project Developers**.
Asistencia de desarrollo: **OpenAI Codex / GPT-6.1 Sol**, con procedencia en DEVELOPMENT.md.

Apache-2.0. Se conservan [LICENSE](LICENSE) y los avisos de terceros en [NOTICE](NOTICE).
El proyecto no está afiliado a OpenAI ni al equipo de Typst.
Documentación original: [README.upstream.md](README.upstream.md).

## English

Independent Typst fork maintained by Jean Porcel for hybrid SVG export to SVG editor.
Compatible Latin runs become editable text objects; unsupported text uses outlines.
Imported SVGs become grouped vectors with unique local IDs. Raster assets are embedded;
unsafe SVG imports use a warned embedded 300 ppi PNG fallback.

Build with `cargo build --release --locked -p typst-cli`, then run:

```sh
./target/release/typst compile document.typ 'page-{0p}.svg' --format svg --svg-text hybrid
```

Install the referenced fonts in your SVG editor. Text objects follow Typst runs; paragraph
reflow is outside this exporter’s scope. Open the SVG and save the SVG.
Development assistance is credited to OpenAI Codex / GPT-6.1 Sol; see DEVELOPMENT.md.
Apache-2.0 with upstream and third-party notices retained.
