# Meshble Web — navigable design-system mockups

Three complete, switchable design systems for the Meshble admin UI, so the look-and-feel can be
chosen by **navigating** real screens rather than picking from descriptions.

- **Linear** — *Meshble Mono*: dark-first, monochrome, high-density, hairline borders, 13px/36px rows.
- **Stripe** — *Meshble Aurora*: light, airy, soft layered shadows, blurple accent, 15px/48px rows.
- **Notion** — *Meshble Notion*: light, warm grays, rounded, near-flat, friendly, 15px/52px rows.

Each system is a complete token set (palette, type, radius, shadow, density) with **light + dark**
variants, generated from independent design specs and contrast-checked (text/bg ≥ 4.5:1).

## Run

```bash
cd web
npm install
npm run dev      # http://localhost:5180
```

No backend needed — the screens run on in-memory mock data whose shapes mirror the real API
(a Sales Order with **inlined line items** = `find_one_secured`, and a computed `amount_total` =
the aggregate compute).

## What to look at

- **Top-right switcher**: flip between Linear / Stripe / Notion live; the sun/moon toggles light/dark.
  Every screen restyles with zero per-theme code — components only read semantic CSS variables.
- **Dashboard → Sales Orders → click a row → Order detail**: the master-detail screen (header record
  + inline order lines + computed total) is the one to judge — it's the core ERP shape.

## How it's wired

- `src/index.css` — the three token systems as CSS variables under `[data-theme][data-mode]`.
- `tailwind.config.js` — semantic colors (`bg`, `surface`, `accent`, …) map to those variables.
- `src/theme.tsx` — theme/mode state on `<html data-theme data-mode>`, persisted to localStorage.
- `src/ui.tsx` — theme-agnostic primitives (Button, Card, Badge, DataTable, …).
- `src/screens/*` — Dashboard, Orders, OrderDetail, Customers, Products.

Once a system is chosen, the unpicked token blocks are deleted and the winner becomes the real UI's
design system (then wired to the live API instead of mock data).
