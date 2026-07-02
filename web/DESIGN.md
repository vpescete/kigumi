# Kigumi Design System — Graphite (DESIGN.md)

> The blend: **structure from shadcn/ui, finish from Untitled UI, identity from Graphite.**
> Hand-rolled React primitives, zero new dependencies, 100% token-driven so all themes inherit every change automatically. No hard-coded hex in any component.

## 1. Direction

Graphite stays **dark, dense, and console-grade**. We do not move toward marketing-site spacing. What changes is **polish and API completeness**:

- **shadcn contributes the bones** — `Button` gains the canonical variant matrix (`default / secondary / outline / ghost / destructive`) × size set (`sm / md / lg / icon`), explicit `hover / active / focus-visible / disabled` states, a semantic input token (`--color-input` + `--color-input-border`, the shadcn `--input` convention), and one unified ring-offset focus. Restraint: small radii, subtle fills, no gradients, single cyan accent.
- **Untitled UI contributes the finish** — a real resting shadow (`--shadow-xs`) that seats controls, a soft card shadow (`--shadow-sm` rebuilt), an **accent-ring + soft-halo** focus (`--shadow-focus`), a subtle press (`active:translate-y-px`), pill badges with a crisp status dot, and a faintly-tinted table header over a crisp divider.

Every value is a CSS variable or a token-reading Tailwind class wired in `tailwind.config.js`. Swapping themes recolors the ring, halo, pills, elevation, and input surface with zero per-component edits.

## 2. Where tokens live (read this before editing)

The theme engine (`src/theme/css.ts`) **emits** these per active theme from each `Theme` object: `--radius-*`, `--shadow-sm/md/overlay`, `--control-h`, `--density-row`, `--fs-base`, `--space`, the `--color-*` palette, and the `t-*` roles. So a token in that set is **per-theme by design** (Swiss is intentionally flat / 0-radius; Editorial is softer). Two consequences:

- **Structural tokens** (`--control-h`, `--radius-*`, `--shadow-sm`) are changed in **both** `src/index.css :root` (the pre-JS Graphite fallback) **and** the Graphite theme object `src/theme/themes/graphite.ts`. Editing only `:root` would be overridden the moment any theme loads. Other themes keep their own identity untouched.
- **Component-system tokens** (`--shadow-xs`, `--shadow-focus`, `--shadow-focus-danger`, `--color-input`, `--color-input-border`, `--color-ring-offset`) are **not** emitted by `css.ts`, so they live only in `:root` and apply to **all** themes. They are written as `var()`/`color-mix()` of existing palette tokens, so they recolor per theme automatically.

## 3. Token decisions

### 3a. Structural (Graphite theme + `:root` fallback)

| Token | From | To | Why |
|---|---|---|---|
| `--control-h` | `30px` | `32px` | Untitled breathing room, still ERP-dense; propagates to every control reading `var(--control-h)`. `--density-row` stays 35px so tables stay dense. |
| `--radius-sm` | `3px` | `4px` | 3px reads nicked; 4px is the deliberate shadcn `sm` for chips/dots/hit-targets. |
| `--radius-md` | `5px` | `6px` | The canonical control radius on a 32px control. Polished-neutral, still crisp. |
| `--radius-lg` | `8px` | `10px` | Softer Untitled container corner; keeps a two-step containment ramp (4 / 6 / 10) so nested controls visibly sit inside panels. |
| `--shadow-sm` | `0 1px 0 0 rgba(8,11,14,0.4)` | `0 1px 3px 0 rgba(3,6,9,0.5), 0 1px 2px -1px rgba(3,6,9,0.5)` | The flat hairline becomes a true soft two-layer card shadow. Cards lift gently; still subtle on `#0d1014`. |

### 3b. Component-system (`:root` only, theme-adaptive, all themes)

| Token | Value | Why |
|---|---|---|
| `--shadow-xs` | `0 1px 2px 0 rgba(3,6,9,0.55), 0 0 0 1px rgba(3,6,9,0.4)` | Resting elevation for buttons/inputs — a 1px drop + hairline contact ring so controls feel seated, not flat. |
| `--shadow-focus` | `0 0 0 4px var(--color-accent-soft)` | The Untitled soft halo — a 4px accent-tinted glow outside the crisp ring. Recolors per theme via `--color-accent-soft`. |
| `--shadow-focus-danger` | `0 0 0 4px var(--color-danger-bg)` | Same halo for destructive buttons / invalid fields, tinted red without hard-coding. |
| `--color-input` | `var(--color-surface2)` | Dedicated field surface (shadcn `--input`). Today = surface2; can diverge per theme later without touching components. |
| `--color-input-border` | `color-mix(in srgb, var(--color-border), var(--color-text) 12%)` | One step lighter than `--color-border`, **derived** (not a fixed hex) so it adapts to light themes too: fields read as slightly raised vs structural dividers. Also the input hover/focus edge. |
| `--color-ring-offset` | `var(--color-bg)` | Makes the ring-offset gap color explicit/semantic so components use a bare `ring-offset-2` and the gap follows the theme. |

> `--fs-base` (13px), `--density-row` (35px), the `--color-*` accents, and the `t-*` roles are **unchanged** — density and identity hold.

## 4. Component conventions

### Focus — ONE treatment, everywhere
`focusRing` in `ui/cx.ts` is the single source: the shadcn ring + `ring-offset-2` on bg **plus** the Untitled halo (`shadow-focus`). Buttons, inputs, combobox, tabs, dialog-close, toast-dismiss inherit it. `focusRingDanger` is the destructive/invalid twin. Never hand-write `focus:ring-2 focus:ring-[var(--color-ring)]` again — that inline string is deleted from every field site.

### Button — variant × size matrix
- **Variants:** `default` (cyan primary), `secondary` (filled neutral), `outline`, `ghost`, `destructive`. `primary` is kept as a back-compat alias of `default`.
- **Sizes:** `sm`, `md` (default), `lg`, `icon`. Height is token-driven off `--control-h` via inline `style` (no `h-*` pinning): `sm` = `calc(var(--control-h) - 4px)`, `md` = `var(--control-h)`, `lg` = `calc(var(--control-h) + 4px)`, `icon` = square `var(--control-h)`.
- **States:** `shadow-xs` resting seat, `active:translate-y-px` press, `disabled:opacity-50 disabled:pointer-events-none`, focus via `focusRing` (destructive uses `focusRingDanger`).
- **`type` defaults to `submit`** (native `<button>` behavior) so buttons inside a `<form>` submit it; pass `type="button"` for in-page actions.
- `confirm()` in `Dialog.tsx` keeps its buttons inlined (not `<Button>`) to avoid a `ui.tsx ↔ ui/Dialog` import cycle, but matches the same visual spec.

### FieldInput — one shared `cls`, the input token, unified focus
A single `cls` string drives text/number/date/datetime/selection/many2one-fallback: `bg-input border-input-border shadow-xs`, hovers the border up, focuses with `focus-visible:border-accent` + the shared ring + halo, and appends an `aria-[invalid=true]` danger border + halo. The boolean toggle keeps `h-6 w-11`, gains `shadow-xs`, and uses the shared `focusRing`. The Combobox wrapper mirrors the field treatment via `focus-within:`.

### Card — softer corner, soft elevation (mechanically free)
`bg-surface border border-border rounded-lg shadow-sm` is unchanged *classes* — but `rounded-lg` is now 10px and `shadow-sm` is the new soft shadow, so cards lift automatically. `interactive` cards add `hover:shadow-md hover:border-input-border`. DataTable's Card and Dialog inherit the rounder corner for free.

### Badge — true status pill with a crisp dot
`rounded-full` pill, soft tinted fill + tinted border per tone (`neutral / success / warning / danger / accent`). `StateBadge`'s leading dot is `h-1.5 w-1.5 rounded-full bg-current` (crisp). The `accent` tone uses the soft `bg-accent-soft text-accent` status-chip look, not the solid button look.

### DataTable — crisp divider, faint header tint, inset row focus
Header row gains a faint `bg-surface2/40` tint over the crisp `border-b border-border`. Body rows keep `var(--density-row)` (35px). Clickable rows: `hover:bg-surface2`, and focus uses an **inset** ring so it never bleeds past the row edge — the one place we deliberately do not use the haloed `focusRing`.

## 5. Theme safety

Verified across themes: under **Swiss** (light, 0-radius, red accent) the radius stays sharp (the bump is scoped to Graphite, not forced globally) and every component-system token recolors correctly (input surface, lighter border via `color-mix`, header tint, pills, halo). Community themes inherit polished focus, halo, pills, elevation, and input surface with no extra work; a theme may override any new var to retune. No hard-coded hex in any component — `npm run build` + a contrast check in both light and dark before shipping a token change.
