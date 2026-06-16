# Roadmap verso Meshble v1 (funzionante)

> Piano per arrivare a una v1 *realmente usabile* da un team interno: auth completa, modelli base,
> gestione utenti/gruppi, sicurezza come dato, un verticale business di prova (Sales), frontend sulla
> API live, e ops/deploy. Costruito con una survey reale del codice + planning multi-agente.
> Stato di partenza: motore ORM, persistenza, migrazioni, engine di sicurezza (ACL/record rule/Ctx),
> auth JWT, data API + OpenAPI/UI-contract, compute+aggregati, relazioni (read inline + nested
> create), **config tipizzata (fatto)**, frontend Graphite + theming (su mock data).

Le decisioni che richiedono approvazione sono in fondo (D1–D10) e in memoria `[[v1-open-decisions]]`.

---

## Milestone

### M0 — Shell eseguibile: CLI unica, entrypoint server, auth lifecycle reale
**Obiettivo:** trasformare la libreria dogfooded in qualcosa che si avvia, migra, seeda e si opera con
un comando — e rendere l'auth lifecycle davvero *server-enforced*. Solo wiring di pezzi esistenti →
rischio minimo, target eseguibile continuo per le milestone successive.
- `apps/meshble-cli` (clap): `serve` (Settings → Db::connect → migrate moduli → router_with_data),
  `migrate`, `config check|print` (assorbe il bin attuale), `user create|set-password|grant`, `version`.
- Bootstrap admin + seed idempotente promossi fuori da renderer-demo: schema auth, gruppi, admin da
  `MESHBLE_ADMIN_PASSWORD` (fail-fast se assente, **mai** admin/admin hardcoded), `--demo-data` opzionale.
- Refresh endpoint → `claim_refresh` (rotazione jti) e logout → `revoke_refresh` (oggi persistiti ma
  inutilizzati → revoca reale, non lato-client). `GET /auth/me`. `/health` + `/ready`.
- CORS configurabile (tower-http); proxy Vite in `web/` verso il server reale.

**Exit:** `meshble migrate && meshble serve` parte su Postgres fresco, bootstrappa l'admin da env, e il
flusso `login → /auth/me → refresh (vecchio jti rifiutato al riuso) → logout (token revocato) →
/health/ready` è corretto. Renderer-demo ridotto a esempio sottile sulla CLI.

### M1 — Read path: search / filter / sort / pagination end-to-end
**Obiettivo:** rendere ogni lista usabile su dati reali. Il Domain AST ha già tutti gli operatori e il
traversal dotted; manca solo che il list handler lo usi. Massima leva, rischio minimo, sblocca il FE.
- Parser query-param → Domain (operatori suffisso `field__gte=` + escape JSON-domain), validato sul
  modello (campo/tipo ignoto → 400). `order_by` (validato su `has_column`, direzione da enum chiuso) +
  `limit`/`offset` in `find_secured`. Envelope paginato (`data`, `total` via `count_secured` sullo
  STESSO dominio sicuro, `limit`, `offset`), riflesso in OpenAPI. ORDER BY solo da identificatori del
  modello (anti-injection).

**Exit:** `GET /api/:name?state=draft&amount__gte=100&order=-id&limit=20&offset=40` → envelope paginato
filtrato da ACL+rule con `total` corretto; tentativo di injection → parametro bound/400; campo ignoto → 400.

### M2 — Fondamenta dati base: res.currency / res.partner / res.company, settings runtime, sequenze
**Obiettivo:** completare i modelli base + due primitive (config runtime, numerazione gapless), facendo
ORA le scelte strutturali costose-dopo (convenzione `company_id`, money decimale).
- `res.currency` (code/symbol/rounding/decimal_places/active/position); `res.partner` (is_company,
  parent_id self-ref, email/phone/address, currency_id, active); `res.company` (+ convenzione FK
  `company_id`, `Ctx.company_id`, record rule same-company globale; una company seedata).
- `meshble_setting(key,value,type)` store runtime + API tipizzata (il DB è l'autorità, OPERATIONS §2.1);
  `ir.sequence` (`meshble_sequence` + `next_value()` con advisory lock per-code, no_gap opt-in,
  prefix/suffix/padding). `rust_decimal` → NUMERIC per i monetari.

**Exit:** migrate fresco produce i tre modelli completi con una company; `next_value('SO')` concorrente
no_gap è gapless; `meshble_setting` round-trippa valori tipizzati ed è autorità su TOML per i runtime;
i monetari sono decimali esatti.

### M3 — Correttezza in scrittura: default, vincoli UNIQUE/CHECK, comandi x2many
**Obiettivo:** scritture corrette/complete per form master-detail editabili — il gap funzionale maggiore.
- Default di campo (`#[field(default=…)]`, applicato prima della validazione required, esposto nel UI
  contract). Vincoli UNIQUE (singolo/composito) e CHECK nel metamodel + DDL; mappa 23505/23514 → 409
  con messaggio curato (mai testo Postgres grezzo). x2many write-through (create/update/delete dei figli
  nella transazione del padre, ri-controllando ACL+record rule del figlio ad ogni ramo). Semantica
  record-rule in scrittura esplicita e testata (una riga creata deve soddisfare la sua Create/Write rule).

**Exit:** create che omette un default ok; duplicato su unique → 409; un singolo PATCH crea/edita/elimina
righe figlie in una transazione con sicurezza per-figlio; insert che viola una rule è respinto in-tx.

### M4 — Compute aggregato transitivo + azioni di transizione di stato
**Obiettivo:** chiudere i gap di compute e lifecycle che servono al verticale Sales.
- Eager-load dei figli O2m in scrittura → aggregati (`amount_total = sum(line_ids.price_subtotal)`,
  margine) corretti e atomici. Endpoint azione `POST /api/:name/:id/action/:name` (fn registrata sotto
  ACL+rule; `#[action]` minimale; draft→sale→done con guardie, sequenza al confirm, readonly-when-done).
  Cascata aggregati multi-livello limitata (cap profondità + cycle detection) sul `recompute_parent`
  advisory-locked.

**Exit:** aggiungere/editare/togliere una riga aggiorna totale+margine atomicamente; il confirm assegna
sequenza, `state=sale`, blocca l'edit di `amount_total` a done; un rollup a due livelli resta coerente.

### M5 — Verticale Sales quote-to-order (la prova)
**Obiettivo:** il verticale piccolo-ma-reale che esercita ogni sottosistema insieme. Sales è l'unico
già parzialmente scaffoldato.
- `sale.order.line` (oggi `line_ids` punta a un target inesistente); catalogo minimo
  `product.product/template` (list_price, standard_cost per margine, currency_id); ACL+rule per i nuovi
  modelli; usa sequenza (SO/2026/0001), compute transitivo (M4), endpoint azione.
- `apps/sales-demo`: migra base+product+sales, seeda valute/partner/prodotti + `sales.user`/`sales.manager`,
  serve CRUD sicuro + azioni + renderer; test E2E (login user → quote → righe → asserisci subtotale/totale/
  margine → confirm → asserisci che le rule nascondono un ordine done/grande al junior + 403 su delete).

**Exit:** il flusso quote-to-order gira live e come test verde; ruoli junior/manager vedono righe diverse.

### M6 — Sicurezza come dato: res.users/res.groups/ir.model.access/ir.rule + loader da DB
**Obiettivo:** rendere identità e autorizzazione *dato amministrabile* invece di array Rust e una colonna
CSV. Fondamento per team multi-ruolo e schermate admin. Dopo il verticale, così il modello di sicurezza
è informato da un'app reale e la migrazione auth-critica è fatta con cura.
- Membership gruppi + `implied_ids` (junction models o Many2many — **D1**); `res.users` (off dalla
  tabella flat, partner_id, active, company_id; preserva i refresh token); `res.groups` con ereditarietà;
  relazione user↔group al posto della colonna CSV; espansione transitiva implied_ids nel Ctx a login/refresh
  (cycle-guard). `ir.model.access` + loader ACL da DB; `ir.rule` + loader rule da DB; `Domain::from_json`
  per rule admin-autorate come dato; `base.security` (gruppi default, ACL default usable-but-locked,
  guardia contro la rimozione dell'ultimo admin).

**Exit:** utenti/gruppi(ereditarietà)/ACL/rule sono righe CRUD-abili; un edit admin ha effetto entro una
finestra via reload hook; la migrazione `meshble_user→res.users` ri-deriva identici i gruppi effettivi;
il sistema rifiuta di togliere l'ultimo grant admin.

### M7 — Hardening identità: password lifecycle, reset, rate-limit/lockout, audit, API key, rotazione JWT, field-level write
**Obiettivo:** portare l'auth a grado ERP interno.
- Password policy + self-service `POST /auth/password` (revoca i refresh); reset admin (token monouso,
  scadenza, constant-time); rate limiting + lockout in Postgres (pre-argon2, enumeration-safe); audit
  auth (`meshble_auth_audit`); API token/service account (`mshb_`-prefix, hashed, scope, revocabili);
  rotazione JWT (verifica su {current, old} via `jwt_secret_old`); **field-level WRITE security**
  (read-hiding differito, **D6**).

**Exit:** un cambio password revoca i refresh; N login falliti lockano enumeration-safe; ogni evento auth
è auditato; un service account chiama l'API con `mshb_` ed è revocabile; token con secret ruotato verifica
nella finestra; una scrittura non autorizzata di campo → 403.

### M8 — Storage + allegati + backup/restore/neutralize (durabilità operativa)
**Obiettivo:** istanze sicure da operare e clonare.
- `meshble-storage`: `BlobStore` + `FsBlobStore` streaming content-addressed (hash-verified, sharded,
  dedup), backend dalla Config; S3 dietro feature flag (**D10**). `ir.attachment` via BlobStore (sha256,
  link res_model/res_id) attraverso CRUD sicuro; upload/download streaming.
- `meshble db dump/restore/verify` + `blobs gc` (tar manifest+pg_dump+blobs, journal fail-closed,
  temp-DB+swap con fallback in-place — **D8**, gate head-match, verify referenziale, GC mark-sweep);
  motore di neutralizzazione (default-ON al restore, dopo le migrazioni, blank dei segreti + mode=dev
  salvo `--production-clone`); envelope di cifratura `age` (**D10**).

**Exit:** dump→restore riproduce DB+blob con integrità; un crash a metà restore lascia l'istanza
fail-closed; un clone non-prod parte neutralizzato; verify/gc reclamano solo i blob non referenziati.

### M9 — Frontend sulla API live: renderer generico + schermate Sales + admin
**Obiettivo:** Graphite off mock data, su API sicura, con architettura **ibrida** (renderer guidato dai
metadati + override sottili per-modello, **D7**).
- `api/client.ts` (envelope tipizzato + loop refresh-and-retry-once sul 401); AuthProvider/RequireAuth;
  login; `/auth/me`. Layer metadati (`useModels`/`useViewContract` + valutatore Domain-AST client per
  invisible/readonly). Widget kit (display+edit) su primitive Graphite; `RecordList` generico
  (filter/sort/paginate da M1 + override colonne per-modello); `RecordForm` generico con tabelle O2m
  editabili che postano i comandi x2many (**D4**). `name_get` per Many2one (picker/colonne mostrano nomi).
  Navigazione dai metadati `/api/models` gated by group; schermate Sales (Dashboard/Orders/Detail con
  azioni) + admin Users/Groups/ACL/Rules/API-keys/Audit. Stati loading/empty/error/optimistic mappati
  su `ApiError`. Playwright E2E sui flussi critici.

**Exit:** un utente fa login e guida il flusso Sales reale (liste paginate/filtrate, dettaglio con righe
inline e relazioni con nome, edit righe con totali live, confirm) e un admin gestisce utenti/gruppi/ACL/
rule — tutto sul server live, con E2E Playwright verde.

### M10 — Ship hardening: Docker, CI, coverage, docs
**Obiettivo:** impacchettare, gateare e documentare la v1.
- Dockerfile multi-stage (cargo-chef) + docker-compose (meshble + postgres:16 healthcheck + volumi,
  incl. volume blob); entrypoint `migrate && serve`; client postgres per dump/restore. GitHub Actions
  (fmt, clippy -D, build, test workspace con Postgres service, coverage `cargo-llvm-cov` con soglia
  ratchet — **D9**, docker-build). Uplift test su ops; README quickstart EN + `GETTING_STARTED.md` +
  runbook OPERATIONS + CLI reference (UX strings English-only per regola).

**Exit:** `docker compose up` dà un'istanza funzionante bootstrappata da env; le PR sono gateate;
un nuovo dev fa clone → compose up → bootstrap admin → prima chiamata API → crea un modulo dalla guida.

---

## Decisioni che richiedono approvazione

> Sintetizzate e deduplicate dai planner. **(consigliato)** = raccomandazione del planning.

| Id | Impatto | Decisione | Quando | Raccomandazione |
|----|---------|-----------|--------|-----------------|
| **D1** | alto | Membership gruppi + `implied_ids`: Many2many di prima classe ora, **junction models** ora (migrabili a M2M dopo), o mezza-misura CSV→tabella | M6 | **Junction models ora** (dato identico, M2M dopo come miglioramento) |
| **D2** | alto | Multi-company in v1: **una company + convenzione `company_id` + rule same-company**, multi-company UX completa, o nessun concetto company | M2 | **Single company + convenzione + rule**, UX multi-company differita |
| **D3** | alto | Money computato: f64, **rust_decimal→NUMERIC** con rounding da currency, o interi (centesimi) | M2 | **rust_decimal→NUMERIC** (fondamento corretto, no rewrite) |
| **D4** | medio | Encoding comandi x2many (contratto write): tuple Odoo, **oggetti tipizzati** `{op,id,values}`, o euristica id-presence | M3 | **Oggetti tipizzati** (coerente con l'ethos typed-not-stringly) |
| **D5** | medio | Sintassi filtro liste: solo JSON domain, solo operatori suffisso, o **entrambi** (suffisso default + JSON escape) | M1 | **Entrambi** (JSON quasi gratis, suffisso copre il 90%) |
| **D6** | medio | Field-level security v1: read+write completa, **solo write** (read-hiding dopo), o nessuna | M7 | **Solo write** (integrità è il rischio maggiore ed è contenuto) |
| **D7** | medio | Origine colonne-liste/menu/azioni: mappe client per-modello, contract server esteso, o **ibrido** (client ora, migra nel contract dopo) | M9 | **Ibrido** (sblocca la UI, fonte di verità migra in Rust) |
| **D8** | medio | Portabilità restore: temp-DB+swap, in-place dietro journal, o **entrambi con auto-detect** | M8 | **Entrambi auto-detect** (managed PG non permette RENAME DATABASE) |
| **D9** | basso | Gate coverage CI: 80% globale, **80% sui crate di logica + carve-out per ops IO**, o track-senza-gate | M10 | **80% logica + carve-out ratchet** |
| **D10** | basso | Blob/cifratura v1: solo Fs non cifrato, **Fs + envelope age** (S3 dopo), o S3+cifratura | M8 | **Fs + age**, S3 dietro feature flag |

### Note di decisione (autonomia)
Per la regola "procedi su ciò che puoi decidere da solo", **D1** (junction models) e **D3**
(rust_decimal) sono decisioni tecniche con risposta chiara: le adotto come da raccomandazione salvo tua
obiezione. Restano da scegliere insieme le decisioni di **prodotto/contratto/infra**: D2, D4, D5, D7 (le
più precoci e impattanti) ora; D6, D8, D9, D10 si confermano alla rispettiva milestone (raccomandazione
già fissata in memoria).
