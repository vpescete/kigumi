# Kigumi — design del metamodello (Rust)

> Rifondazione della base di Odoo in Rust. Obiettivo: stesso valore (metamodello
> dichiarativo + estendibilità per composizione), senza i difetti strutturali
> (vedi [`ANALISI_ODOO19.md`](./ANALISI_ODOO19.md)).
>
> Tre vincoli-guida, imposti dall'utente:
> 1. **Community-friendly** — basso attrito, stack mainstream, niente ecosistemi isolati.
> 2. **Agnostico** — il core non impone frontend, né protocollo, né client.
> 3. **Integrabile** — ogni cosa è esposta via standard aperti generati dallo schema.

## 0. Principio cardine

Una sola **definizione di modello** è l'unica sorgente di verità. Da essa si **generano**,
a build time, quattro proiezioni:

```
                    ┌─────────────► schema DB + migrazioni (Postgres)
   #[model]         ├─────────────► API: OpenAPI/REST + GraphQL + JSON Schema
   definizione ────►├─────────────► contratto UI agnostico (JSON) → qualsiasi frontend
   (Rust)           └─────────────► policy di security (ACL + row-level)
```

Differenza chiave con Odoo: in Odoo queste proiezioni sono **interpretate a runtime** da
mutazione non tipizzata (`type()`, `safe_eval`, `xpath`). Qui sono **risolte e validate a
compile time**. Un riferimento rotto, un'estensione su un punto inesistente, un dominio mal
tipizzato → **errore di compilazione**, non di produzione.

---

## 1. Perché "agnostico" cambia l'architettura

Odoo accoppia server e client sulla semantica (il web client OWL conosce modelli, domini,
onchange). Questo blocca client alternativi. Kigumi inverte:

- **Il core è headless.** Non sa nulla di HTML/React/OWL. Espone **solo** API tipizzate +
  un **contratto UI dichiarativo in JSON** (lista campi, widget suggerito, layout, regole di
  visibilità). Qualsiasi frontend (React, Svelte, mobile nativo, un altro ERP) lo consuma.
- **Niente framework frontend proprietario.** Il valore di Odoo non è OWL: è il layer
  dichiarativo che descrive la UI. Quel layer qui è **dati JSON**, non codice.
- **Protocollo non imposto.** REST+OpenAPI è il default (massima integrabilità), ma lo stesso
  schema genera GraphQL e, opzionalmente, un canale realtime. Un sistema terzo integra via
  OpenAPI come con qualsiasi API moderna.

```
┌── kigumi-core (Rust, headless) ──────────────────────────┐
│  Registry (Arc, condiviso, no GIL)                        │
│  ORM set-based · domini AST tipizzati · security engine   │
└───┬───────────────┬────────────────┬─────────────────────┘
    │ OpenAPI/REST  │ GraphQL         │ UI-contract (JSON)
    ▼               ▼                 ▼
 SDK generati   integrazioni     frontend a scelta
 (TS, Py, Go)   di terzi         (React/Svelte/mobile/...)
```

Conseguenza pratica per la **community**: un dev frontend usa il suo stack e i suoi tool
(Vite, devtools, librerie). Un dev di integrazioni riceve un **SDK tipizzato generato**
nella sua lingua. Nessuno deve imparare un framework proprietario per essere produttivo.

---

## 2. La definizione di modello — `#[model]`

Sorgente di verità. Esempio `sale.order` (forma target):

```rust
use kigumi::prelude::*;

#[model(name = "sale.order", table = "sale_order")]
pub struct SaleOrder {
    #[field(string = "Order Reference", required, index, default = expr("new"))]
    pub name: Field<String>,

    #[field(string = "Customer", required)]
    pub partner_id: Many2one<"res.partner">,

    #[field(string = "Order Lines")]
    pub line_ids: One2many<"sale.order.line", "order_id">,

    #[field(string = "Status", default = "draft")]
    pub state: Selection<&[("draft","Draft"),("sale","Confirmed"),("done","Done")]>,

    // Computed STORED con dipendenze dichiarate e VERIFICATE dal compilatore:
    // se `line_ids.price_subtotal` non esiste, è errore di build, non N+1 a runtime.
    #[field(string = "Total", compute = "compute_amount", store, currency = "currency_id")]
    #[depends("line_ids.price_subtotal")]
    pub amount_total: Field<Decimal>,

    #[field(string = "Currency", required)]
    pub currency_id: Many2one<"res.currency">,
}

impl SaleOrder {
    // INVARIANTE: gira SEMPRE (create/write/import/API), non solo dalla form.
    fn compute_amount(&self, ctx: &Ctx) -> Computed<Decimal> {
        Computed::sum(self.line_ids(ctx).map(|l| l.price_subtotal))
    }

    // ASSIST UI: funzione PURA, compilabile anche a WASM → niente round-trip al server.
    #[onchange("partner_id")]
    fn assist_partner(rec: &Draft<SaleOrder>) -> Suggestions {
        Suggestions::set("currency_id", rec.partner_id.property_currency())
    }

    // Azione: tipizzata, non `safe_eval` di una stringa.
    #[action(label = "Confirm", groups = "sales.group_user")]
    fn confirm(&self, ctx: &Ctx) -> Result<()> {
        self.write(ctx, set!{ state: "sale" })
    }
}
```

### Cosa espande la macro (contratto)
`#[model]` genera, a compile time:
1. La struct concreta + un `impl Model for SaleOrder` con un **descrittore statico**
   (`ModelDescriptor`) ispezionabile: campi, tipi, relazioni, compute, dipendenze, azioni.
   → l'opposto della classe runtime di Odoo: qui `kigumi describe sale.order` stampa la
   definizione *risolta*.
2. Le funzioni di accesso tipizzate (`self.line_ids(ctx) -> RecordSet<SaleOrderLine>`).
3. La registrazione nel `Registry` via `inventory` (raccolta a compile time, vedi §4).

`#[field]`, `#[depends]`, `#[onchange]`, `#[action]` sono attributi inerti letti dalla macro:
niente magia a runtime.

---

## 3. Estensione per composizione — risolta a compile time

Il superpotere di Odoo (`_inherit`) senza il caos. Un modulo estende un modello **dichiarando
un'estensione**, non monkey-patchando una classe:

```rust
// Nel modulo `sale_margin`: aggiunge un campo a sale.order definito altrove.
#[extend("sale.order")]
pub struct SaleOrderMargin {
    #[field(string = "Margin", compute = "compute_margin", store)]
    #[depends("amount_total", "purchase_cost")]
    pub margin: Field<Decimal>,
}

#[extend_impl("sale.order")]
impl SaleOrderMargin {
    fn compute_margin(&self, ctx: &Ctx) -> Computed<Decimal> { /* ... */ }
}
```

Risoluzione (a build time, nel crate che compone i moduli):
- Tutte le `#[extend("sale.order")]` vengono raccolte e **fuse in un unico `ModelDescriptor`
  risolto**. Esiste un punto dove la definizione finale è materializzabile e ispezionabile.
- **Confini espliciti**: un modello dichiara cosa è estendibile (default: campi additivi sì,
  override di comportamento solo su punti marcati `#[extension_point]`). Estendere un punto
  non dichiarato → **errore di compilazione**.
- **Niente ordine fragile**: il grafo dei moduli è risolto topologicamente e i conflitti
  (due moduli che ridefiniscono lo stesso campo in modo incompatibile) sono **errori di build**.

Questo elimina in un colpo: niente "go to definition" → ora c'è; "stessa config, comportamento
diverso" → la composizione è deterministica e ispezionabile; refactor cieco → il compilatore
verifica ogni `depends` e ogni accesso a campo.

---

## 4. Registry condiviso (no GIL)

```rust
pub struct Registry {
    models: HashMap<ModelId, Arc<ResolvedModel>>,  // immutabile dopo il boot
    // ...
}
// Un solo Registry per processo, condiviso da tutti i task async:
type SharedRegistry = Arc<Registry>;
```

- I modelli si auto-registrano a compile time con il crate `inventory` (o `linkme`):
  zero codice di wiring, come il manifest di Odoo ma **verificato dal linker**.
- Async runtime (Tokio): migliaia di richieste concorrenti su tutti i core, **un solo
  Registry** in RAM (vs N copie nei worker prefork di Odoo).
- Invalidazione cache **granulare e real-time** (signaling via Postgres `LISTEN/NOTIFY`
  mantenuto come idea, ma push immediato invece di "ricarica al prossimo request").

---

## 5. Le quattro proiezioni generate

### 5.1 DB + migrazioni
Dal `ResolvedModel` → DDL Postgres. Le migrazioni sono **generate e diffabili** (come
`sqlx`/`sea-orm` migrations), versionate, idempotenti. External ID (idempotenza dei dati seed)
mantenuto come tabella `ir_model_data` equivalente.

### 5.2 API: OpenAPI/REST + GraphQL
Dallo stesso descrittore → spec **OpenAPI 3.1** completa (questo è il cuore dell'"integrabile":
chiunque integra come con una qualsiasi API REST documentata) e uno schema **GraphQL**. Da
OpenAPI si **generano SDK tipizzati** (TS, Python, Go) per i client — niente più "client ORM
mirror" accoppiato come in Odoo.

### 5.3 Contratto UI agnostico (JSON)
Niente XML interpretato, niente OWL. Una **vista è un documento JSON** prodotto dal descrittore:
```json
{
  "model": "sale.order", "type": "form",
  "fields": [
    {"name": "partner_id", "widget": "many2one", "required": true},
    {"name": "amount_total", "widget": "monetary", "readonly": true}
  ],
  "layout": [{"group": ["partner_id", "currency_id"]}, {"slot": "line_ids"}],
  "rules": [{"invisible": {"field": "state", "op": "=", "val": "draft"}}]
}
```
- **Estensione per ancore semantiche nominate** (`"slot": "line_ids"`), **non** per xpath sul
  DOM → niente rottura agli upgrade (il difetto #1 lamentato dai dev Odoo).
- Le **regole di visibilità/readonly sono dati**, non espressioni eval nel markup → testabili,
  condivisibili client/server.
- Qualsiasi frontend renderizza questo JSON con i suoi componenti. Il core non sa che frontend
  esista.

### 5.4 Security (ACL + row-level)
- ACL CRUD per gruppo (come `ir.model.access`) ma **dichiarata sul modello/azione e
  verificata** — niente CSV slegato.
- Row-level come **domini AST tipizzati** (non `safe_eval` di stringhe come `ir.rule`),
  compilati nel `WHERE`. `sudo` → **escalation esplicita e tracciata** nel tipo (`Ctx::elevated`),
  non un metodo facile da abusare.

---

## 6. Mappa: difetto Odoo → soluzione Kigumi

| Difetto Odoo (file:riga) | Soluzione |
|---|---|
| Classe via `type()` runtime (`model_classes.py:179`) | `ResolvedModel` risolto a compile time, ispezionabile |
| No type-safety sui campi | Tipi concreti (`Many2one<"...">`), il compilatore è il refactoring |
| `_inherit` monkey-patch globale | `#[extend]` con confini dichiarati, fusione verificata a build |
| N+1 silenziosi | `#[depends]` verificato; query planner che li marca a test time |
| onchange solo UI (`models.py:6973`) | invariante (sempre) vs assist UI puro (anche WASM) |
| view xpath fragile (`ir_ui_view.py:944`) | UI = JSON, estensione per ancore semantiche |
| `ir.rule` `safe_eval` (`ir_rule.py:70`) | domini AST tipizzati → SQL, no eval di stringhe |
| RPC implicito non versionato | OpenAPI/GraphQL generati, SDK tipizzati |
| OWL isolato | core headless + frontend mainstream a scelta |
| wkhtmltopdf morto | report engine moderno (Typst / Chromium headless) — fuori dal core |

---

## 7. Layout del workspace (cargo)

```
kigumi/
├── Cargo.toml                 # workspace
├── crates/
│   ├── kigumi-core/          # Registry, ORM traits, Ctx, RecordSet, domini AST
│   ├── kigumi-macros/        # proc-macro: #[model], #[field], #[extend], #[action]
│   ├── kigumi-schema/        # ResolvedModel → DDL, OpenAPI, GraphQL, UI-contract
│   ├── kigumi-db/            # persistenza Postgres (sqlx): DDL + query parametrizzate dal Domain
│   ├── kigumi-auth/          # auth: verifica JWT HS256 (Bearer) → Ctx fidato
│   ├── kigumi-server/        # axum: serve OpenAPI spec, model list, contratti-UI, CRUD dal catalogo
│   └── kigumi/               # facade: prelude, re-export
├── modules/
│   └── sales/                 # primo modulo applicativo (sale.order) — dogfooding
└── docs/
```

Stack scelto (tutti mainstream → community-friendly): **tokio** (async), **axum** (HTTP),
**sqlx** (Postgres, query verificate a compile time), **serde** (JSON), **utoipa** (OpenAPI),
**async-graphql** (GraphQL), **inventory** (registrazione modelli).

---

## 8. Roadmap (lazy: walking skeleton prima, magia dopo)

1. **Walking skeleton** (questo commit): `Model` trait + `ResolvedModel` descrittore scritti
   *a mano* (no macro ancora), un modello `sale.order`, generazione del DDL e del UI-contract,
   un endpoint REST read. Prova end-to-end che il metamodello → 3 proiezioni regge. *Compila.*
2. ~~**Proc-macro `#[model]`**: sostituisce la definizione a mano.~~ ✅ Fatta — `kigumi-macros`
   genera `ModelDescriptor` + `impl Model` dal DSL della struct; output identico alla fase 1.
3. **`#[extend]` + risoluzione composizione**. ✅ Fatto: `#[extend]` + auto-registrazione via
   `inventory` + `resolve_registered()` (catalogo modelli) **e** `resolve_module_set()` /
   `resolve_modules()` (grafo moduli: compat framework + range SemVer + topo-order, indurito da
   audit adversarial). Vedi `VERSIONING.md`.
4. **Domini AST tipizzati + security engine**. ✅ Fatto: `Domain` AST → SQL **parametrizzato**
   con validazione contro il modello (no `safe_eval`, no injection); ACL + record rules
   (global AND / group OR) + `Ctx`/`sudo` (flag privato). Indurito da audit security adversarial
   (5 fix: su forgiabile, NULL three-valued ×2, NaN, gate operatore↔kind). **UI**: le regole
   `invisible_when`/`readonly_when` sono nel contratto-UI come **AST JSON portabile**
   (`Domain::to_json`), validate contro il modello — stesso domain che il server compila in SQL.
5. **OpenAPI/GraphQL + generazione SDK**. 🟡 In corso: proiezione **OpenAPI 3.1**
   (`kigumi_schema::openapi`) generata dal catalogo modelli (schemas + paths) → spec consumabile
   da chiunque, SDK via `openapi-generator`. Server headless `kigumi-server` (axum):
   metadata (`/openapi.json`, `/api/models`, `/api/{m}/view`) **+ CRUD dati sicuro**
   (`GET/POST/PATCH/DELETE /api/{m}[/{id}]`): ogni operazione passa per ACL + record rules
   (`*_secured` di `kigumi-db`), input validato a confine (required/option/kind), errori
   403/400/404/500-opaco. **Auth**: JWT HS256 (`kigumi-auth`, crate pluggable) — `Authorization:
   Bearer` verificato (firma + alg pinned + exp) → `Ctx` fidato; assente/invalido → 401 (prima
   del DB). **Lifecycle**: `/auth/login` (password **argon2**, login a tempo-costante),
   `/auth/refresh` (refresh token **stateful**: revoca+rotazione atomica, no double-spend),
   `/auth/logout` (revoca); access/refresh **tipizzati** (un refresh non vale come bearer).
   Indurito da audit (null-bypass, timing enumeration, race rotazione; auth-token boundary: 0). ⬅ resta **GraphQL**.
6. **Persistenza reale (sqlx) + migrazioni generate**. ✅ Fatto: `kigumi-db` (crate separato,
   backend pluggable — il core resta headless) esegue il DDL generato e le query con `WHERE`
   compilato dal `Domain` e **parametrizzato** (anti-injection provato a runtime), con read
   **security-enforced** (ACL + record rules). **Migrazioni versionate** (`install_or_upgrade`,
   tabelle `kigumi_module`/`kigumi_migration`): atomiche (transazione), serializzate
   (`pg_advisory_xact_lock`), idempotenti, con check duplicati e reachability. Indurito da audit
   adversarial (leak errori, race, gap di versione).

Il punto 1 è nel codice del workspace; il resto sono fasi successive da validare una alla volta.
