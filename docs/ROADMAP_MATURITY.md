# Roadmap Maturity — post-analisi RFC "Road to Maturity"

> Esito della verifica punto-per-punto (2026-07-17) dell'RFC esterno contro il codice reale:
> 5 punti parziali (in gran parte già costruiti), 1 mancante ma rimandato con cognizione, 3 contrari
> al design. Questa roadmap raccoglie **solo ciò che ha senso portare**: i delta reali dell'RFC più i
> gap veri che l'RFC non ha visto. Ordinata per leva e rischio, non per numerazione RFC.
> F1–F4 parallelizzabili dopo F0, con due eccezioni annotate: F3 parte dopo il fix `amount_type`
> di F1, e F2/F4 presuppongono il `ServeOptions` non_exhaustive che entra nella 0.2.0 di F1.
> Ogni claim di questo documento è stato ri-verificato adversarialmente contro il sorgente.

## Respinti (non entrano in roadmap)

- **CQRS/Event Sourcing (RFC 2.1)** — l'outbox è un log di integrazione, non un event store
  (`change_summary` senza i valori dei record — solo nomi campo, con la sola eccezione from/to
  dello stato — ed è troncabile); il GL deriva già i saldi dal giornale a read-time; `stock.quant`
  deve restare una riga lockabile per il motore di prenotazione. Il residuo utile (transazionalità
  dell'evento delete) è in F1; i seam best-effort restanti (`post_move`, `register_payment`) sono
  annotati in F3.
- **Multi-DB / CockroachDB (RFC 2.2)** — "Postgres IS the queue" (`event_schema.rs:96`) è una
  scommessa deliberata: advisory lock sul write path, cursore SSE su `xid8`, bigserial ovunque.
  Se servirà HA: Postgres gestito, non un layer di astrazione.
- **Frontend WASM Leptos/Yew (RFC 4.2, ramo A)** — `METAMODEL_DESIGN.md:36-39` rifiuta
  esplicitamente il framework frontend proprietario; ~7.5k righe React attive e contract-driven.
- **Peppol/SDI adesso (RFC 1.2)** — post-v1, guidato da un deployment italiano reale.
  I prerequisiti contabili (repartition, campi fiscali partner) maturano in F3; numerazione
  davvero gapless e inalterabilità richiedono una decisione owner che rovescia `button_draft`.

---

## F0 — Igiene di rilascio (subito, ~1 giorno)

**Problema:** i crate su crates.io sono del 2-3/07 e mancano di **~14 commit** fino a HEAD:
non solo l'ondata Tier-2 (tracing e53f014, i18n e08d591, portal b11bb65, SSO e94ecf8) ma anche
test-kit (1761bc0), MCP v2 (3ae654a), API keys (1faf0e0+3e0e998), MCP-over-HTTP (936a5ef+67b63c6),
hardening (17ed439), S3 (60f1f58). Il quickstart del README (`cargo install kigumi-cli`) installa
una build senza tutto questo. Nessun git tag esiste.

- **Fix `publish-all.sh` prima di tutto**: oggi salta i crate già presenti a *qualsiasi* versione
  (check di esistenza, `publish-all.sh:16-20`) — rieseguirlo dopo il bump non pubblicherebbe
  nulla. Passare a un check per-versione (`/api/v1/crates/$crate/versions`).
- Bump workspace `0.1.0 → 0.1.1`, `kigumi-cli 0.1.1 → 0.1.2`; ripubblicare; **git tag** per ogni
  versione pubblicata, d'ora in poi sempre. Additività verificata a livello API (diff
  058ea4f..HEAD: zero firme `pub` rimosse/cambiate; nuove chiavi config tutte `#[serde(default)]`),
  ma le **release note devono segnalare tre cambi di comportamento**: (1) auth può rispondere 503
  sotto carico (load-shed Argon2); (2) login diventato case-insensitive (account duplicati per
  case collidono in modo non deterministico); (3) il guest delle route `auth:false` ora porta il
  gruppo `public` — collisione di nome per adopter che avessero già un gruppo chiamato così.
- Fix documentazione falsa/stale: `.env.example` (rimuovere `KIGUMI_JWT_SECRET_OLD` "accepted on
  verify" — non cablato in nessun Authenticator — e `KIGUMI_ADMIN_TOKEN` — dump/restore/gc non
  esistono); `web/README.md:28` ("mock data" — falso) e contestuale trim di `web/src/data.ts`
  (mock morti; restano solo `STATE_LABEL`/`fmtMoney` da rilocare); `ROADMAP_V1.md:8`; docstring
  uninstall "Applies on restart" (`kigumi-server/src/lib.rs:984-985` — il codice fa live-refresh).

**Exit:** `cargo install kigumi-cli` installa HEAD; ogni versione su crates.io ha un tag; nessun
doc descrive comportamenti inesistenti.

## F1 — Correttezza e superficie API (≈1-2 settimane → release 0.2.0)

Fix piccoli ma reali; alcuni breaking → si raccolgono in una **0.2.0**. Il treno di rilascio è
più di una riga: riscrittura dei range `framework` nei 5 manifest (`>=0.2, <0.3`), **e** bump +
ripubblicazione dei 5 crate modulo (che pinnano `kigumi ^0.1` nelle Cargo deps), di runtime, mcp
e cli — una sola run di publish ordinata e taggata (dipende dal fix publish-all.sh di F0).

- **Bug OpenAPI path**: lo spec emette `/api/{table}` (`kigumi-schema/src/openapi.rs:16-18`) ma le
  route risolvono per **nome** (`/api/sale.order`): ogni path con table ≠ name è un 404 nel
  contratto. Fix: chiavare su `m.name`.
- **Catalogo autenticato**: oggi `/openapi.json`, `/api/models` e `GET /api/:name/view` sono
  anonimi sul router dati (`lib.rs:770-784, 1088`). Fix raccomandato: il catalogo segue le ACL —
  autenticato di default, un modello visibile al guest solo se il gruppo `public` ha una ACL di
  Read (coerente col portal primitive; niente flag di config). **Nella stessa PR**: aggiornare i
  curl del template README dello scaffolder (`scaffold.rs:414` oggi stamperebbe un quickstart che
  401-a) e i quickstart di `docs/guida/{en,it}/api.md`. Consumer in-repo verificati safe: SPA
  (catalogo dietro identity gate), MCP (in-process), renderer-demo (token deep-link);
  `webui/app.html` fa un 401 pre-login accettato (il catch guida già al login).
- **`amount_type` sconosciuto → errore**: oggi cade silenziosamente nel ramo percent
  (`modules/sales/src/tax.rs:97-100`); validare in `resolve_tax_specs` con errore esplicito.
  Prerequisito del seam estensibile di F3.
- **Validazione override servizi a boot**: `service_for` è first-match in link-order senza check
  di duplicati (`kigumi-db/src/service.rs:82-85`), a differenza delle route. Decidere la semantica
  di override (per i moduli di localizzazione di F3) e validare a boot — è un cambio core, va in
  questa 0.2.0.
- **`ServeOptions` non_exhaustive + Default** (`kigumi-runtime/src/lib.rs:131-138`): oggi ogni app
  scaffoldata lo costruisce come literal esaustivo → qualunque campo nuovo (`[telemetry]` di F2,
  `ui` di F4) sarebbe un compile-break per tutti gli adopter. Breaking una volta sola qui, con
  `..Default::default()` nel template; F2/F4 diventano puramente additive.
- **Pruning delle code** — con i vincoli veri, non un clone di `gc_done_activities`:
  `webhook_delivery.outbox_id` è `ON DELETE CASCADE` (`event_schema.rs:77`), quindi mai potare una
  riga outbox con delivery pendenti (i retry arrivano legittimamente a giorni di attesa; le righe
  `dead` sono il forensic record). Predicato: prima `webhook_delivery` (sent/dead oltre
  retention), poi `event_outbox` solo se `dispatched` e senza delivery residue; per le righe mai
  dispatchate (nei runtime adopter il fan-out non gira: parte solo nel binario CLI,
  `main.rs:752,758`) retention separata e più lunga, con la scelta documentata. `kigumi_job`
  (done/dead) e `kigumi_refresh` (revocati/scaduti) sono i casi semplici. Documentare nella guida
  API che un `Last-Event-ID` più vecchio della retention salta silenziosamente (coerente col
  contratto events-are-hints).
- **Transazionalità dell'evento delete** — scope onesto: avvolgere DELETE + cleanup polimorfici
  (attachment/thread) + `enqueue_event_in_tx` richiede di sciogliere la tolleranza
  "unmigrated-table" che oggi ingoia gli errori (non compone in una tx: o savepoint, o si
  *richiede* lo schema eventi — difendibile, `ensure_event_schema` gira a ogni migrate/serve).
  I recompute dei parent restano post-commit (finestra di staleness documentata) oppure adottano
  `acquire_agg_locks_ordered`; `post_move`/`register_payment` restano best-effort (lato moduli,
  nota in F3).
- **CORS configurabile** (residuo M0): `tower-http` CorsLayer da `[server]`; oggi solo proxy Vite.

**Exit:** spec OpenAPI eseguibile contro il server live; catalogo non anonimo (test dedicato) con
quickstart aggiornati; tabelle di coda con retention che non cancella mai delivery pendenti;
delete transazionale; tassa con `amount_type` ignoto → 400; servizio duplicato → errore a boot;
app scaffoldata compila invariata dopo un campo nuovo in `ServeOptions`.

## F2 — Osservabilità: chiudere il layer già pianificato (1-2 settimane, crate core)

Il substrato c'è (e53f014); il commit stesso dichiara "OTLP export is the opt-in next layer".
Presupposto: il `ServeOptions` non_exhaustive di F1 (la sezione `[telemetry]` è additiva).
Due work item distinti, in ordine:

1. **Traces (economico)**: rifattorizzare `init_tracing` (`kigumi-server/src/lib.rs:603`) da
   `fmt().try_init()` a composizione `registry().with(fmt).with(otel)`; feature opt-in `otel`
   (`tracing-opentelemetry` + `opentelemetry-otlp`, precedente: feature `s3` di kigumi-storage);
   sezione `[telemetry]` in config (endpoint OTLP); span sotto l'HTTP (`#[instrument]` sui percorsi
   secured di kigumi-db e su job/cron/webhook in runtime); estrazione `traceparent` in ingresso.
2. **Metrics (da zero, separato)**: counter/histogram per richieste HTTP, job, delivery webhook,
   SSE; export via OTLP metrics allo stesso collector. Niente exporter per-backend: Prometheus/
   Jaeger/Grafana si raggiungono dal collector.

Vincolo: l'instrumentazione vive nei **crate** core (server/db/runtime), mai in `modules/*`
(split framework/ERP). Attenzione al peso dello stack opentelemetry in CI (disco già andato in
overflow una volta, fix 81690f5) — per questo la feature è opt-in.

**Exit:** con `otel` attiva e collector configurato, una richiesta HTTP produce una trace
end-to-end (HTTP → db → trigger); metriche base visibili in Prometheus via collector; senza
feature, zero dipendenze aggiunte.

## F3 — Profondità fiscale: repartition per-tassa (2-4 settimane, lato moduli; dopo il fix `amount_type` di F1)

L'unico delta vero dell'RFC 1.1. Oggi il GL posta **tutte** le tasse su un unico conto trovato con
`first_match(account_type="tax")` (`modules/account/src/services.rs:177-181, 236-245`): ritenuta
d'acconto e reverse charge sono *irrappresentabili*, non solo assenti.

- **Proprietà di `account.tax` → decisione owner DM5.** Il piano annotato nel codice ("il modulo
  account ADOTTA via `#[extend]`", `modules/sales/src/lib.rs:370-373`) ha una conseguenza non
  scritta: `#[extend]` richiede il modello base linkato, quindi account acquisirebbe una dipendenza
  Cargo+manifest da sales — inversione di layering che contraddice il design dichiarato dei servizi
  account ("no compile-time dep sugli order model", `account/services.rs:2-4`), e con
  `module_of(account.tax)=sales` disinstallare sales farebbe sparire la configurazione fiscale
  mentre account resta installato. **Raccomandazione: spostare il `#[model]` in account tenendo la
  tabella `account_tax`** (module_of passa ad account, zero migrazione dati; sales dichiara
  `depends account` — la direzione Odoo naturale) e restringere la ACL attuale che dà a
  `sales.manager` CRUD pieno sulle tasse (`modules/sales/src/lib.rs:710-711`), che dopo la
  repartition significherebbe editare i mapping di postazione GL.
- **Repartition lines** (mappa tassa → conto GL con segno/fattore, stile Odoo semplificato):
  `create_invoice`/`create_vendor_bill` passano da bucket-per-gruppo a righe per-tassa.
- Con le repartition diventano **rappresentabili come configurazione** (dati, non codice):
  ritenuta (riga negativa verso conto erario), reverse charge (doppia riga ±IVA in autofattura).
  Nessuna logica nazionale hardcodata: resta tutto data-driven, coerente col design.
- **Seam estensibile per `amount_type`**: registry locale al modulo (non trait object in core),
  appoggiato alla validazione override di F1; aggiungere anche la validazione a boot delle
  `#[extend]` pendenti (oggi un'estensione verso un modello non linkato è **ignorata in silenzio**,
  `registry.rs:474-483`).
- Tax tags/grids per reporting IVA: **minimo o rinviato** — serve davvero solo con un modulo l10n.
- Nota: `post_move`/`register_payment` mantengono l'enqueue eventi best-effort (vedi F1);
  se serve atomicità piena è lavoro qui, sui servizi del modulo.

Rischi noti: disciplina di rounding documentata (subtotal+tax == gross esatto) e i test
d'integrazione tax_* esistenti come rete di sicurezza.

**Exit:** una tassa con repartition posta su conti distinti; ritenuta e reverse charge dimostrati
con sole righe di configurazione in un test; `create_invoice` senza assunzione mono-conto;
`#[extend]` verso modello assente → errore a boot.

## F4 — Admin out-of-the-box + chiusura D7 (2-3 settimane)

L'architettura è fatta (renderer contract-driven, provato da due client); manca il packaging e la
coda della decisione D7 ("tutto nel contract server, il renderer FE non ha mappe per-modello").

- **Servire la SPA dal binario** — il meccanismo va pinnato: feature `ui` su **kigumi-server**
  (non nel CLI: la registrazione è statica a link-time, quindi l'app scaffoldata serve se stessa
  via `kigumi_runtime::serve`, non via `kigumi serve`), ri-esportata da kigumi-runtime e abilitata
  nel template dello scaffolder. I byte: copia in-crate della dist buildata (web/dist ≈ 412K, ben
  sotto il cap dei 10MB; precedente in-crate: i font genpdf), con step `npm build` nel flusso di
  publish e fallback vuoto a feature spenta (docs.rs deve compilare).
- **Menu nel contract**: concetto di menu lato server (derivato dai moduli + override), via la
  euristica client su prefissi ERP (`web/src/nav.ts:15-37`) — oggi è una violazione dello split
  dentro il client.
- **Advertise di service actions / wizard / smart buttons nel contract** usando i registri
  esistenti; eliminare i tre registry client per-modello (`web/src/registries/*` — i loro stessi
  commenti si dichiarano seam temporanei) **più gli altri siti hardcoded**: il lookup
  `account.journal` in `ModelForm.tsx:542`, la lista KPI di `Dashboard.tsx`, e la rilocazione di
  `STATE_LABEL`/`fmtMoney` fuori da `data.ts` (trim già in F0).
- **UX liste**: paginazione (il server già espone `{data,total,limit,offset}`) e ricerca libera
  (`ilike` già supportato dai suffissi D5); oggi limite fisso 80 (`ModelList.tsx:43`) senza
  controlli.
- Smoke E2E Playwright su login → lista → form → azione (criterio di uscita M9 mai chiuso).

**Exit:** `kigumi new && cargo run -p app -- serve` → pannello utilizzabile senza toccare il repo
web; `grep -rE "sale\.|account\.|product\." web/src` pulito fuori da test/fixture; liste paginate
e cercabili.

## F5 — Verso 1.0: enforcement prima del freeze (in coda, decisione owner)

L'RFC ha il problema invertito: i moduli sono già 1.0.0, è il **framework** a essere 0.1.x. Il
vero evento 1.0 è congelare il contratto framework (prelude ~135 item ri-esportati, 24 macro
`register_*!`, metamodello, envelope HTTP + UI contract) — già pianificato in
`VERSIONING.md:40-45`. Prematuro congelare ora (5 cambi di superficie nelle ultime due settimane);
giusto preparare l'enforcement:

- `cargo-semver-checks` (o `cargo public-api`) in CI sui crate pubblicati; snapshot-test del
  UI contract e dell'envelope HTTP come contract-tests.
- Lint di allineamento deps Cargo ↔ deps manifest (TODO riconosciuto,
  `modules/sales/Cargo.toml:19-21`).
- `kigumi new --module`: modalità module-only dello scaffolder (delta RFC 3.1; i template
  `MODULE_TOML`/`MODULE_LIB_RS` esistono già in `scaffold.rs:209-305`) — utile all'ecosistema
  prima del freeze.
- Audit della superficie stabile (`non_exhaustive` dove serve — `ServeOptions` anticipato in F1 —
  sealed traits) → poi la decisione di data per 1.0 + branch LTS.

**Exit:** una PR che rompe la superficie pubblica fallisce in CI; scaffold di un modulo standalone
in un workspace esistente; documento "superficie stabile" pronto per la decisione 1.0.

## F6 — Debito della roadmap interna (già pianificato in V1, qui solo riordinato)

In ordine di valore, dopo o in parallelo alle fasi sopra; nessuno è un item RFC ma sono i veri
gap di maturità:

1. **M6** — gruppi come junction (`res.users`/`res.groups` reali, `implied_ids`), migrazione off
   CSV flat: il gap aperto più grosso della roadmap V1.
2. **M7 residuo** — password self-service + reset, lockout per-account su Postgres, tabella audit
   auth; cablare la rotazione dual-secret JWT (`jwt_secret_old` è già in config, mai usato).
3. **M8** — `kigumi db dump/restore/verify`, `blobs gc` (il design adversarially-reviewed è già
   in `OPERATIONS.md`; oggi zero codice).
4. **M10** — CI: fmt/clippy gate, coverage con ratchet (decisione D9), Dockerfile + compose.
5. Test dove sono a zero: `kigumi-runtime` (runtime di ogni app scaffoldata), frontend `web/`.

---

## Decisioni owner richieste (stile D1-D10)

| # | Decisione | Raccomandazione |
|---|---|---|
| DM1 | Versioning release: 0.1.1 subito (additiva, con le 3 note di comportamento) poi 0.2.0 con i breaking di F1 e il treno completo (5 moduli + runtime/mcp/cli)? | Sì: due release separate, tag entrambe |
| DM2 | Catalogo: auth-only secco o visibilità guest ACL-driven? | ACL-driven (coerente col portal primitive) |
| DM3 | Metrics: OTLP metrics via collector o endpoint /metrics Prometheus nativo? | OTLP via collector (un solo percorso di export) |
| DM4 | Timing 1.0: dopo F5 con superficie ferma per N settimane? | Enforcement prima, data solo a superficie stabile |
| DM5 | Proprietà `account.tax`: account→sales via `#[extend]` (dep invertita) o `#[model]` spostato in account con tabella invariata e `sales depends account`? | Spostare il modello in account (direzione Odoo, zero migrazione dati) |
