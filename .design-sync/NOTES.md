# design-sync NOTES — meshble UI primitives

`meshble-web` (`web/`) is an **app, not a published component library**. The synced design system is the
standalone primitives in `web/src/ui/` (Dialog, Combobox, CommandPalette, Tooltip, Tabs, Toast, Skeleton,
Sparkline). The `web/src/ui.tsx` barrel components (Button/Card/DataTable/StateBadge…) are coupled to
`./data` (app state) and are intentionally NOT synced.

## Build setup (package shape, synth-from-src)

- **No dist library.** Do NOT let the converter synth from the WHOLE `src/`: it does `export *` from every
  file, dragging in `App.tsx`/`main.tsx`, whose top-level `createRoot(document.getElementById('root'))`
  throws at bundle-eval and leaves `window.MeshbleUI` undefined (`[BUNDLE_EXPORT] 8/8 not a component`).
  **Fix (in place):** a dedicated entry `web/src/ds-bundle-entry.ts` re-exports ONLY the 8 primitives, set
  as `cfg.entry`. Keep that file in sync if primitives are added/removed — it is the bundle's export surface.
- **PKG_DIR:** `meshble-web` is not self-installed under `node_modules`; `cfg.entry` (a file under `web/`)
  makes PKG_DIR walk up to `web/`. (A self-symlink `web/node_modules/meshble-web -> ..` also works but is
  unnecessary with `cfg.entry`; it's under gitignored `node_modules`.)
- **Build command:** run from repo root —
  `node .ds-sync/package-build.mjs --config .design-sync/config.json --node-modules web/node_modules --out ./ds-bundle`
  (no `--entry` flag needed; `cfg.entry` carries it). Re-copy `.ds-sync/` from the skill first.
- **cssEntry is a HASHED filename** (`web/dist/assets/index-<hash>.css`, today `index-BlfpPYLK.css`) produced
  by `npm run build` in `web/`. If the app CSS is rebuilt and the hash changes, update `cfg.cssEntry`.
- **Render check (validate):** no playwright is bundled in `.ds-sync`. Install it without downloading a
  browser, then point it at the system Chrome:
  `cd .ds-sync && PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm i playwright`, then run validate with
  `DS_CHROMIUM_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`.

## Fonts

Space Grotesk / Hanken Grotesk / JetBrains Mono are **Google-hosted** (the meshble app loads them via a
`<link>`, not `@font-face`). `cfg.runtimeFontPrefixes` suppresses `[FONT_MISSING]`. The DS pane renders in
fallback fonts unless the brand fonts are shipped — see Re-sync risks.

## Previews

- 6 components ship the **floor card** (Combobox, CommandPalette, Dialog, Sparkline, Tabs, ToastProvider);
  `Skeleton` renders live (no required props). `Tooltip` has an **authored** preview
  (`.design-sync/previews/Tooltip.tsx`) because it is hover-driven and rendered blank otherwise — the card
  shows its triggers.
- Known render warns: none (render check clean, 8/8).

## Re-sync risks (watch-list)

- **`cfg.cssEntry` hash drift** — the single most likely break. Re-run `web` build and update the hash.
- **`ds-bundle-entry.ts` staleness** — adding/removing/renaming a primitive needs a matching edit there, or
  it won't appear on `window.MeshbleUI`.
- **Floor-card enrichment** — the 6 floor-card components can be upgraded to authored previews on any
  re-sync (`.design-sync/previews/<Name>.tsx`); authored files + grades carry forward.
- **Fonts not shipped** — to render the DS pane in the real brand fonts, add `@font-face` + woff2 via
  `cfg.extraFonts` (or a Google Fonts remote `@import`); today it falls back to system fonts.
- **Tooltip preview uses inline styles with `var(--*)` tokens** tied to the current token names; if a token
  is renamed in `web/src/index.css`, update the preview.
