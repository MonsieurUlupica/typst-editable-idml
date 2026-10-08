# Contributing to Typst Editable SVG

This guide applies to Jean Porcel’s independent SVG fork. Describe reproducible
problems or proposals in an issue before large refactors. Include small public
Typst sources and original SVG assets needed to reproduce export problems.

Keep changes focused on SVG export, document behavior and run:

```sh
cargo fmt --all -- --check
cargo test --locked -p typst-svg
cargo test --locked -p typst-cli --test smoke
```

Disclose model assistance, review generated code and explain your reasoning.
Do not submit client documents or private assets as fixtures.
Manual SVG editor reports should include app version, OS and installed fonts.
The upstream project has its own rules in CONTRIBUTING.upstream.md.
