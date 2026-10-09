# Contributing to Typst Editable IDML

This guide applies to this independent Typst fork and its IDML exporter. Open an issue before large changes. Include small public Typst sources and original assets needed to reproduce a problem; do not submit client documents or private assets.

Keep changes focused and run the relevant checks:

```sh
cargo fmt --all -- --check
cargo test --locked -p typst-idml
cargo test --locked -p typst-cli --test smoke
```

For package or visual behavior, describe the operating system, InDesign version, installed fonts and whether you tested opening, editing and resaving the IDML. Disclose model assistance, review generated code and explain your technical reasoning.

The original Typst contribution policy is preserved in [CONTRIBUTING.upstream.md](CONTRIBUTING.upstream.md). It applies to submissions to upstream Typst; this independent fork has its own review process.
