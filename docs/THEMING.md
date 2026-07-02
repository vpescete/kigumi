# Theming — temi della community

Kigumi è **theme-able come Odoo, ma più leggero**: un tema NON è un modulo con override SCSS +
template + asset bundle da ricompilare. Un tema è **dato dichiarativo** che il framework trasforma in
variabili CSS a runtime. I componenti UI leggono solo variabili semantiche (`--color-*`) e ruoli
tipografici (`.t-*`) — non sanno quale tema sia attivo, quindi aggiungere un tema non tocca una riga
di UI.

Tema base: **Graphite**. Gli altri (Editorial, Swiss, Verdigris, Mono-Tech) sono la libreria seed.

## Il theme contract

Un tema rispetta `web/src/theme/contract.ts` (`interface Theme`):

| Campo | Cosa |
|---|---|
| `id`, `name`, `author`, `version`, `compat` | identità; `version` SemVer, `compat` range framework (es. `^0.1`) |
| `defaultMode` | `light` \| `dark` |
| `fontImports[]` | URL stylesheet dei font (es. Google Fonts), iniettati on-demand |
| `fonts` | `display` / `body` / `mono` (font-family stack) |
| `type` | 8 ruoli (`display`,`h1`,`h2`,`subtitle`,`body`,`label`,`caption`,`mono`), ognuno con `stack`/`size`/`weight`/`lh`/`tracking`/`transform` |
| `radius`, `shadow`, `density` | forma e densità |
| `color` | `light` + `dark`, i **17 token semantici** (bg, surface, surface2, border, text, textMuted, accent, accentFg, accentHover, accentSoft, success(+Bg), warning(+Bg), danger(+Bg), ring) |

## Tre modi per creare un tema

1. **Theme Studio** (nessun codice) — pagina `/theme-studio` nell'app: forka un tema base, modifica
   token con anteprima live sulle schermate vere, controllo contrasto in tempo reale, poi **Save**
   (entra nello switcher) o **Export JSON**.
2. **Drop-in JSON** (nessun rebuild) — metti un `*.theme.json` (conforme al contract) in
   `web/public/themes/` e aggiungilo a `web/public/themes/index.json`. All'avvio l'app lo carica, lo
   valida e lo mostra nello switcher. Vedi `midnight-rose.theme.json` come esempio.
3. **Built-in TS** — un `Theme` in `web/src/theme/themes/` registrato in `index.ts`. Type-safe,
   spedito col bundle (come i 5 seed).

## Regole (validazione automatica)

`web/src/theme/validate.ts` applica:
- **struttura**: id kebab-case, tutti i ruoli e i 17 token presenti in entrambe le modalità;
- **colori sicuri**: solo hex/rgb/hsl (un drop-in non può iniettare CSS arbitrario nello `<style>`);
- **contrasto WCAG**: testo/sfondo, testo-muted/surface, label/accent devono essere ≥ 4.5:1 (warning).

I temi invalidi (errori strutturali/colore) vengono rifiutati; i contrasti bassi sono warning nel
Studio.

## Come funziona dietro

`src/theme/css.ts::themeToCss` genera il blocco `[data-theme='id'][data-mode]` e `injectThemes`
lo mette in un unico `<style>` a runtime (font via `<link>` dedotti dai `fontImports`). Il registry
(`src/theme/registry.ts`) unisce built-in + drop-in + custom (localStorage) e notifica lo switcher.
Nessun CSS per-tema statico: un tema della community si comporta **identico** a uno spedito.
