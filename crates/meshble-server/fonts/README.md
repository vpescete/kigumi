# Vendored fonts

`LiberationSans-{Regular,Bold,Italic,BoldItalic}.ttf` — the **Liberation Sans** family, a
metric-compatible substitute for Arial, distributed under the **SIL Open Font License (OFL)**, which
permits bundling and redistribution.

They are embedded (`include_bytes!`) by `src/pdf.rs` so the report PDF rasterizer needs no font files on
disk and no system-font discovery at runtime. Replace them with any OFL/redistributable family by
swapping the four files (keep the names) — the four styles form one genpdf `FontFamily`.
