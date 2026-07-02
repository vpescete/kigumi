# Meshble UI primitives

The standalone UI primitives of the Meshble admin app — a **dark, cyan-accented "precision instrument"** theme (the default theme is *Graphite*). Eight components, exposed on `window.MeshbleUI`: `Dialog` (+ imperative `confirm`), `ToastProvider` (+ `useToast`), `Combobox`, `CommandPalette`, `Tooltip`, `Tabs`, `Skeleton` (+ `SkeletonText` / `SkeletonTable` / `SkeletonStat`), `Sparkline`.

## Setup

- **Theme tokens are global CSS variables.** The bundled `styles.css` defines the default theme (Graphite, dark) on `:root`; every component reads `var(--color-*)`, `var(--radius-*)`, `var(--font-*)`. No theme provider is needed for styling — just keep `styles.css` loaded.
- **Toasts need a provider.** Wrap the tree in `<ToastProvider>` to use `useToast()` (`toast.success(msg)`, `toast.error(msg)`, `toast.info(msg)`). The other seven primitives need no wrapper.
- **Overlays portal to `document.body`** (`Dialog`, `CommandPalette`, and toasts) so they escape ancestor overflow/stacking. `confirm({ title, body })` returns a `Promise<boolean>`.

## Styling idiom

These components are **pre-styled from the theme tokens — you do not pass class names to them** (props carry behavior + content, not styling). For your *own* layout glue around them, use the same tokens so it matches the theme:

- color: `var(--color-bg)`, `--color-surface`, `--color-surface2`, `--color-border`, `--color-text`, `--color-text-muted`, `--color-accent`, `--color-accent-fg`, `--color-success`
- radius: `var(--radius-sm | --radius-md | --radius-lg)` · fonts: `var(--font-body)`, `--font-display`, `--font-mono`

If the host app ships Tailwind, the semantic utilities `bg-surface`, `text-muted`, `border-border`, `text-accent`, `rounded-md` map onto these exact variables — but the variables are the source of truth.

## Where the truth lives

- `styles.css` and its `@import` closure (`_ds_bundle.css`) — the tokens and the component CSS.
- `components/<group>/<Name>/<Name>.prompt.md` — per-component API and usage.

## Example

```tsx
function SaveBar() {
  const toast = useToast()                          // requires <ToastProvider> above
  return (
    <button
      style={{
        background: 'var(--color-accent)',
        color: 'var(--color-accent-fg)',
        padding: '8px 14px',
        borderRadius: 'var(--radius-md)',
        fontFamily: 'var(--font-body)',
      }}
      onClick={() => toast.success('Changes saved')}
    >
      Save
    </button>
  )
}

// at the root: <ToastProvider><SaveBar /></ToastProvider>
```

> Brand fonts (Space Grotesk / Hanken Grotesk / JetBrains Mono) are served by the host app at runtime; the design pane falls back to system fonts unless they are loaded.
