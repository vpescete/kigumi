# Meshble Web — navigable design-system mockups

Five **genuinely distinct** design systems for the Meshble admin UI, chosen by navigating real
screens. Each is a complete token set built with anti-slop taste rules: a distinctive **font pairing**
(never default Inter), a full **type scale per role** (display / h1 / h2 / subtitle / body / label /
caption / mono — different fonts for headings vs body vs numbers), a **built palette** around one
deliberate accent hue (no AI-purple), and its own density, radius and materiality — in **light + dark**.

| System | Fonts (display / body / mono) | Accent | Feel |
|---|---|---|---|
| **Graphite** | Space Grotesk · Hanken · JetBrains Mono | cyan | dark dev-console, dense, hairline |
| **Editorial** | Fraunces (serif) · Hanken · IBM Plex Mono | terracotta | warm magazine, airy, light |
| **Swiss** | Archivo · Libre Franklin · Spline Mono | signal red | bold typographic, flat, grid |
| **Verdigris** | Bricolage Grotesque · DM Sans · DM Mono | emerald | friendly, rounded, soft |
| **Mono-Tech** | IBM Plex Sans · IBM Plex Sans · IBM Plex Mono | amber | ops console, dense, mono-forward |

Contrast was checked (text/bg ≥ 4.5:1) in both modes; the five were verified genuinely distinct
(font, accent hue, density, materiality).

## Run

```bash
cd web
npm install
npm run dev      # http://localhost:5180
```

No backend needed — screens run on in-memory mock data shaped like the real API (a Sales Order with
**inlined line items** = `find_one_secured`, computed `amount_total` = the aggregate compute).

## What to look at

- **Top-right switcher**: flip Graphite / Editorial / Swiss / Verdigris / Mono-Tech live; sun/moon
  toggles light/dark. Everything restyles — colors AND the whole type system — with zero per-theme
  code. Components only read semantic CSS variables and `.t-*` type roles.
- **Dashboard → Sales Orders → click a row → Order detail**: the master-detail screen (header record
  + inline order lines + computed total) is the one to judge — the core ERP shape.

## Community theming

A theme is declarative **data** (`src/theme/contract.ts`), turned into CSS variables at runtime — so
the community can ship themes without touching the UI. Three ways to make one:

1. **Theme Studio** (`/theme-studio`, no code): fork a base, tune tokens with live preview + contrast
   lint, then Save (joins the switcher) or Export JSON.
2. **Drop-in JSON** (no rebuild): add a `*.theme.json` to `public/themes/` + list it in
   `public/themes/index.json` (see `midnight-rose.theme.json`).
3. **Built-in TS**: a `Theme` in `src/theme/themes/` registered in `index.ts`.

Validation (`src/theme/validate.ts`): structure + safe color formats + WCAG contrast (≥ 4.5:1). See
[../docs/THEMING.md](../docs/THEMING.md).

## How it's wired

- `src/theme/contract.ts` — the public, versioned `Theme` shape (tokens + 8 type roles).
- `src/theme/css.ts` — `themeToCss` + runtime injection (built-in & community themes, identical path).
- `src/theme/registry.ts` — built-ins + drop-ins + customs (localStorage), reactive.
- `src/theme/themes/*` — the 5 seed systems as `Theme` objects (Graphite is the base).
- `src/type.css` — `.t-display … .t-mono` role classes driven by per-theme type variables.
- `src/index.css` — `:root` fallback = Graphite dark (resilient before JS); structure only.
- `tailwind.config.js` — semantic colors (`bg`, `surface`, `accent`, `accent-soft`, …) → vars.
- `src/ui.tsx` — theme-agnostic primitives; `src/screens/*` — the screens + Theme Studio.
