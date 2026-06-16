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

## How it's wired

- `src/index.css` — the five token systems (colors + type scale + radius/shadow/density) as CSS
  variables under `[data-theme][data-mode]`, generated from the design specs.
- `src/type.css` — `.t-display … .t-mono` role classes driven by the per-theme type variables.
- `tailwind.config.js` — semantic colors (`bg`, `surface`, `accent`, `accent-soft`, …) map to vars.
- `src/theme.tsx` — theme/mode state on `<html data-theme data-mode>`, persisted, validated.
- `src/ui.tsx` — theme-agnostic primitives (Button, Card, Badge, DataTable, Stat).
- `src/screens/*` — Dashboard, Orders, OrderDetail, Customers, Products.

Once a system is chosen, the unpicked token blocks are deleted, the winner becomes the real design
system, and the screens are wired to the live API instead of mock data.
