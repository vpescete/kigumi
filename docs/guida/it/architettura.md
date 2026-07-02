# Architettura

Kigumi è un framework ERP headless e schema-driven scritto in Rust: un modello è
definito una sola volta come **dato statico ispezionabile** e da quell'unica fonte di
verità il framework deriva lo schema Postgres, il contratto-UI consumabile da qualsiasi
frontend e lo schema OpenAPI per gli integratori. Questa pagina descrive il layout dei
crate e le loro responsabilità, il metamodello e i registri a compile time, la pipeline
di generazione, il ciclo di vita di una richiesta HTTP e il modello di versionamento.
Per la panoramica e il quickstart vedi [README.md](./README.md); per i dettagli di
sicurezza vedi [sicurezza.md](./sicurezza.md) e per le API REST [api.md](./api.md).

## Layout del workspace

Il workspace Cargo raggruppa tre famiglie di membri:

```toml
[workspace]
resolver = "2"
members = ["crates/*", "modules/*", "apps/*"]
```

- `crates/*` — il **framework**: i crate che implementano metamodello, persistenza,
  sicurezza, server. Condividono tutti la stessa versione SemVer del workspace.
- `modules/*` — i **moduli** applicativi (`base`, `mail`, `sales`, `account`, `stock`).
  Ognuno ha la propria versione, indipendente dal framework.
- `apps/*` — le **applicazioni** eseguibili che linkano framework e moduli (`kigumi-cli`,
  `renderer-demo`).

Tutti i crate del framework condividono la versione dichiarata in `[workspace.package]`:

```toml
[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
repository = "https://github.com/vpescete/msh_framework"
```

### Responsabilità dei crate del framework

| Crate | Responsabilità |
|---|---|
| `kigumi-core` | Il metamodello ispezionabile (`FieldKind`, `FieldDef`, `ModelDescriptor`, `ResolvedModel`), i registri a compile time via `inventory`, il motore di sicurezza (ACL + record rule + `Ctx`), il domain AST tipizzato, il modello di versionamento dei moduli. Nessuna dipendenza da database o HTTP. |
| `kigumi-macros` | I proc-macro `#[model]` e `#[extend]`: generano lo `ModelDescriptor` statico + `impl Model` da una struct annotata, ed emettono le registrazioni `inventory` per security a livello di campo (`#[field(groups = "...")]`), related field (`#[field(related = "...")]`), tracked field (`#[field(tracked)]`) e delegazione `inherits` (`#[model(inherits = "...", via = "...")]`). |
| `kigumi-schema` | Le proiezioni da `ResolvedModel`: `to_ddl` (DDL Postgres), `to_ui_contract` (contratto-UI JSON) e `openapi` (schema OpenAPI 3.1). Stessa fonte di verità, output multipli. |
| `kigumi-db` | Lo strato di persistenza Postgres (`sqlx`). Espone i metodi `*_secured` che applicano il motore di sicurezza al confine del database, il motore di migrazioni versionato, gli store di supporto (auth, ACL/record rule a runtime, moduli installati, sequenze, impostazioni, cron). |
| `kigumi-auth` | Autenticazione: hashing password (argon2) e token JWT firmati HS256 (access/refresh tipizzati). Verifica un bearer access token in un `Ctx` fidato. |
| `kigumi-server` | Il router HTTP (`axum`): espone i metadati (OpenAPI, lista modelli, contratti-UI) e gli endpoint CRUD sicuri. Per ogni richiesta dati verifica il token in un `Ctx` e delega la persistenza a `kigumi-db`. |
| `kigumi-config` | Configurazione di istanza tipizzata: impostazioni non-segrete da `defaults < kigumi.toml < env` (validazione fail-fast) e i segreti, letti solo dall'ambiente e verificati all'avvio. |
| `kigumi-storage` | Storage di blob content-addressed: gli allegati binari vivono dietro il trait `BlobStore`, indicizzati dallo sha256 del contenuto (bytes identici deduplicano in un unico oggetto). v1 fornisce `FsBlobStore`. |
| `kigumi` (facade) | La facciata. I moduli applicativi dipendono **solo** da questo crate: `use kigumi::prelude::*;` espone il metamodello, le macro, le proiezioni di schema e tutte le macro `register_*!`. |

La facciata re-esporta anche `inventory`, così le macro possono emettere percorsi
assoluti `::kigumi::inventory::submit!` senza che ogni modulo debba aggiungere la
dipendenza:

```rust
// crates/kigumi/src/lib.rs
pub use kigumi_core::inventory;
```

## Il metamodello

Il cuore del framework è il metamodello in `crates/kigumi-core/src/metamodel.rs`. Un
modello non è una classe sintetizzata a runtime, ma **dato statico ispezionabile**.

### `FieldKind`

Il tipo logico di un campo. Da qui il framework deriva il tipo SQL, il widget UI e il
tipo API:

```rust
pub enum FieldKind {
    Text,
    Html,
    Image,
    Integer,
    Float,
    Decimal { currency_field: Option<&'static str> },
    Bool,
    Date,
    Datetime,
    Selection(&'static [(&'static str, &'static str)]),
    Many2one { target: &'static str },
    One2many { target: &'static str, inverse: &'static str },
    Many2many {
        target: &'static str,
        relation: &'static str,
        column: &'static str,
        target_column: &'static str,
    },
}
```

Note rilevanti per la generazione:

- `Many2one` genera una colonna FK; `One2many` **non** genera colonna (vive sull'inverso);
  `Many2many` non genera colonna sul modello (la membership vive nella tabella di
  giunzione `relation`).
- `Image` è un campo `bigint` FK verso la tabella degli allegati: i byte vivono nel blob
  store content-addressed, indicizzati dallo sha256, e il campo porta l'id dell'allegato.
- `Decimal` porta un `currency_field` opzionale che ne fa un campo "monetary" collegato a
  una valuta; per importi non esatti (quantità, pesi, fattori, tassi) si usa `Float`.

### `FieldDef`

La definizione di un singolo campo:

```rust
pub struct FieldDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub stored: bool,
    pub compute: Option<&'static str>,
    pub depends: &'static [&'static str],
    pub default: Option<&'static str>,
    pub unique: bool,
    pub check: Option<&'static str>,
}
```

Due metodi guidano la generazione: `has_column()` è vero solo se il campo è `stored` e
non è una relazione `One2many`/`Many2many`; `is_computed()` è vero se `compute` è
presente.

### `ResolvedModel`

Un `ModelDescriptor` descrive un modello come definito da **un** modulo (la "base"). Il
`ResolvedModel` è invece il descrittore **risolto**: base più tutte le estensioni unite e
validate.

```rust
pub struct ResolvedModel {
    pub name: &'static str,
    pub table: &'static str,
    pub fields: Vec<FieldDef>,
}
```

La funzione `resolve(base, extensions)` unisce la base con le estensioni dei moduli; un
conflitto di nome di campo è un **errore**, non un override silenzioso. La funzione
`validate_depends` verifica che ogni `depends` punti a un campo esistente (primo segmento
del path), così una dipendenza rotta è un errore di build e non un bug a runtime; rifiuta
inoltre un `depends` relazionale (dotato di punto) su un campo computed non stored, che
verrebbe valutato same-record e leggerebbe silenziosamente vuoto.

### Ispezionabilità

Poiché ogni modello è dato statico, il catalogo è interrogabile a runtime senza
introspezione di classi: `resolve_registered(model)` restituisce il `ResolvedModel`,
`resolve_all_registered()` l'intero set, `registered_model_names()` i nomi (ordinati e
deterministici). Su questi descrittori operano direttamente le proiezioni di
`kigumi-schema`. Per il design completo del metamodello vedi
[`METAMODEL_DESIGN.md`](../../METAMODEL_DESIGN.md).

## I registri a compile time (`inventory`)

I modelli e le estensioni si **auto-registrano** tramite il crate `inventory`: il
resolver li unisce senza wiring manuale. Ogni macro `register_*!` (o l'annotazione
`#[model]`/`#[field]`) emette una `inventory::submit!` di un tipo registrato in
`kigumi-core` (definito in `registry.rs` e nei moduli affini `action.rs`, `report.rs`,
`wizard.rs`, `view.rs`, `security.rs`, e re-esportato dalla facciata). Le macro
`register_*!` vivono nella facciata, in `crates/kigumi/src/lib.rs`. Esempio:

```rust
// crates/kigumi/src/lib.rs
macro_rules! register_module {
    ($manifest:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ModuleRegistration { manifest: || $manifest, crate_path: ::core::module_path!() }
        }
    };
}
```

Ogni tipo registrato ha la sua `inventory::collect!`, e il core fornisce le funzioni di
raccolta che iterano tutte le submission linkate nel binario.

| Registro | Tipo | Emesso da | Raccolto da |
|---|---|---|---|
| Modelli base | `ModelRegistration` | `#[model]` | `registered_model_names`, `resolve_registered` |
| Estensioni di campo | `FieldExtension` | `#[extend]` | `resolve_registered` (unite alla base) |
| Manifest dei moduli | `ModuleRegistration` | `register_module!` | `resolve_modules` |
| ACL | `AclRegistration` | `register_acls!` | `registered_acls` |
| Record rule | `RecordRuleRegistration` | `register_rules!` | `registered_rules` |
| Action | `ActionRegistration` | `register_action!` | `actions_for`, `action_for` |
| Report | `ReportRegistration` | `register_report!` | `reports_for`, `report_for` |
| Wizard | `WizardRegistration` | `register_wizard!` | `wizard_for` |
| View (form) | `FormView` | `register_view!` | `view_for` |
| Modelli con chatter | `MailedRegistration` | `register_mailed!` | `mailed_models`, `is_mailed` |
| Tabelle esterne | `ExternalTable` | `register_external!` | `external_tables` |
| Modelli transient | `TransientRegistration` | `register_transient!` | `transient_models`, `is_transient` |
| Campi tracked | `TrackedFieldRegistration` | `#[field(tracked)]` / `register_tracked!` | `tracked_fields` |
| Delegazione `inherits` | `InheritsRegistration` | `#[model(inherits = …, via = …)]` / `register_inherits!` | `inherits_of`, `delegated_fields` |
| Related field | `RelatedRegistration` | `#[field(related = "...")]` / `register_related!` | `related_path` |
| Security a livello di campo | `FieldGroupRegistration` | `#[field(groups = "...")]` / `register_field_groups!` | `field_required_groups` |
| Compute | `ComputeRegistration` | `register_compute!` | `compute_fn`, `computed_fields` |
| Constraint cross-record | `ConstraintRegistration` | `register_constraint!` | `check_constraints` |
| Cron | `CronRegistration` (in `kigumi-db`) | `inventory::submit!` a mano | `registered_crons` |

`registered_group_names()` deriva i gruppi noti del catalogo unendo quelli referenziati da
qualsiasi ACL o record rule registrata (ordinati, deterministici): è la sorgente per il
seed della lista read-only `res.groups`.

### Risoluzione del catalogo e ordine di migrazione

`resolve_registered(model)` parte dalla base registrata, raccoglie e ordina le estensioni
per modulo (deterministico), le unisce con `resolve` (conflict check) e valida la
delegazione `inherits` e i `depends`. `migration_plan()` produce il piano di migrazione
**ordinato topologicamente** per dipendenze FK: i target FK di un modello — `Many2one` e
`Image` (FK verso la tabella degli allegati) — sono creati prima della tabella
referenziante; un'auto-referenza è ignorata e un ciclo FK genuino è un errore. Le tabelle
esterne (`register_external!`) sono risolte e servite come ogni modello ma **escluse**
dalla migrazione: il metamodello non crea né altera la loro tabella.

## La pipeline di generazione

Le proiezioni vivono in `crates/kigumi-schema/src/lib.rs` e
`crates/kigumi-schema/src/openapi.rs`. Da **un** `ResolvedModel` si producono tre output.

### 1. DDL Postgres — `to_ddl`

`to_ddl(m)` genera il `CREATE TABLE`. La PK `id bigserial` è sempre presente; solo i campi
con colonna (`has_column()`) producono una riga; un `Many2one` aggiunge
`REFERENCES <target>(id)`, un `Image` `REFERENCES kigumi_attachment(id)`; `required`,
`unique` e `check` aggiungono i rispettivi vincoli. La tabella di un nome puntato deriva
sostituendo `.` con `_`.

```rust
pub fn to_ddl(m: &ResolvedModel) -> String {
    let mut lines = vec!["  id bigserial PRIMARY KEY".to_string()];
    for f in m.fields.iter().filter(|f| f.has_column()) {
        // ... pg_type(&f.kind), REFERENCES, NOT NULL, UNIQUE, CHECK ...
    }
    format!("CREATE TABLE {} (\n{}\n);", m.table, lines.join(",\n"))
}
```

La mappatura dei tipi (`pg_type`): `Text`/`Html`/`Selection` → `text`, `Integer` →
`bigint`, `Float` → `double precision`, `Decimal` → `numeric`, `Bool` → `boolean`,
`Date` → `date`, `Datetime` → `timestamptz`, `Many2one`/`Image` → `bigint`;
`One2many`/`Many2many` non hanno colonna.

### 2. Contratto-UI JSON — `to_ui_contract`

`to_ui_contract(m, rules)` produce il contratto-UI: JSON consumabile da **qualsiasi**
frontend. Per ogni campo emette nome, label, widget suggerito, `required` e `readonly`
(i campi computed e related sono read-only); per le `Selection` le opzioni; per le
relazioni `relation`/`inverse`. Le regole dinamiche `invisible_when`/`readonly_when` sono
emesse come AST di domain JSON portabili — **gli stessi** domain che il server compila in
SQL, mai una stringa valutata. Una regola che referenzia un campo sconosciuto è un errore
(non un'UI rotta scoperta in produzione). Il contratto include anche le colonne della
list view, le action disponibili (con i gruppi ammessi), i report stampabili, il flag
`mailed` e la form view dichiarata; i campi delegati via `inherits` sono esposti in modo
trasparente come campi editabili.

### 3. Schema OpenAPI 3.1 — `openapi`

`openapi(models)` costruisce un documento OpenAPI 3.1 (`openapi(&[&ResolvedModel])`) che
descrive i modelli come una REST API documentata; da questo si generano SDK tipizzati
(TS/Python/Go) con tooling standard, senza client scritti a mano. Per ogni modello emette
lo schema dei campi e i path `/api/<table>` (list) e `/api/<table>/{id}` (get-one):

```rust
let base = format!("/api/{}", m.table);
paths.insert(base.clone(), json!({ "get": list_op(m) }));
paths.insert(format!("{base}/{{id}}"), json!({ "get": get_op(m) }));
```

I tipi dei campi seguono la stessa fonte: `Decimal` è serializzato come stringa (formato
`decimal`) per preservare la precisione; un `One2many` è un array che referenzia lo schema
del modello figlio; un `Many2many` un array di id `int64`.

## Il ciclo di vita di una richiesta

Una richiesta dati attraversa una catena precisa, con un **unico punto** in cui ACL,
record rule e multi-azienda sono applicati. Il router è in
`crates/kigumi-server/src/lib.rs`.

### Il router

`router_with_data` costruisce il router completo: rotte di metadati più gli endpoint CRUD
sicuri. La firma esplicita la dipendenza dal segreto JWT:

```rust
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
) -> Router
```

Le rotte CRUD principali registrate:

```rust
.route("/api/:name", get(list_handler).post(create_handler))
.route("/api/:name/:id", get(get_one_handler).patch(update_handler).delete(delete_handler))
.route("/api/:name/:id/action/:action", post(action_handler))
```

più le rotte di autenticazione (`/auth/login`, `/auth/refresh`, `/auth/logout`,
`/auth/me`), di salute (`/health`, `/ready`), e quelle per allegati, chatter, attività,
follower, report e i servizi di business pinnati (ad es. `generate_variants`,
`apply_pricelist`, `apply_discount`, `post`, `create_invoice`, `validate`). Il router
solo-metadati `router(models)` espone `/openapi.json`, `/api/models` e `/api/:name/view`
senza database.

### Passo 1 — HTTP → JWT auth → `Ctx` fidato

Ogni handler dati comincia verificando il bearer token in un `Ctx`:

```rust
fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    backend
        .auth
        .verify_bearer(header)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}
```

`verify_bearer` (in `kigumi-auth`) estrae `Bearer <token>`, lo verifica come **access**
token HS256 (rifiutando i refresh token, con `alg=HS256` pinnato contro alg-confusion) e
lo trasforma in un `Ctx`. I claim trasportano `groups` e lo scope multi-azienda
(`company`/`companies`): un set non vuoto produce un `Ctx` company-scoped tramite
`Ctx::in_companies(active, allowed)`. Questa è autenticazione reale: un client non può
rivendicare un gruppo senza un token firmato dal segreto del server.

Il `Ctx` (in `crates/kigumi-core/src/security.rs`) porta `uid`, `groups`, l'azienda
attiva (`company_id`) e l'insieme di aziende consentite (`allowed_company_ids`), e un flag
superuser **privato** (`su`): codice esterno non può forgiare un contesto elevato con un
literal di struct, perché l'unica via di escalation è il metodo greppabile `Ctx::sudo()`.

### Passo 2 — CRUD sicuro in `kigumi-db`

L'handler delega a uno dei metodi `*_secured` di `Db`
(`crates/kigumi-db/src/lib.rs`), che applicano il motore di sicurezza al **confine del
database**:

| Operazione | Entry point |
|---|---|
| Lista paginata | `list_secured` |
| Conteggio | `count_secured` |
| Righe come JSON | `find_secured` |
| Id visibili | `find_ids_secured` |
| Get-one | `find_one_secured` |
| Create | `insert_secured` |
| Update | `update_secured` |
| Delete | `delete_secured` |

In lettura, il punto unico di enforcement è `secured_read_domain`: verifica l'ACL di Read,
controlla che il filtro fornito dal chiamante non referenzi campi non leggibili (D6,
inclusi i path relazionali percorsi hop-by-hop), poi compone in `AND` il record rule
domain e il filtro del chiamante e vi aggiunge in `AND` la restrizione multi-azienda:

```rust
let rule = record_rule_domain(Operation::Read, model.name, ctx, rules);
let base = match (filter, rule) { /* AND di filtro e regola */ };
Ok(match company_filter(model, ctx) {
    Some(cf) => base.and(cf),
    None => base,
})
```

Il domain risultante è compilato in un `WHERE` **parametrizzato**: i valori sono
sempre bound (`$1, $2, …`), mai interpolati nel testo SQL — chiudendo la superficie di
injection. In scrittura, `insert_secured`/`update_secured`/`delete_secured` verificano
l'ACL della rispettiva operazione, applicano la security a livello di campo
(`check_writable_fields`), e fanno rispettare la record rule e lo scope azienda nella
stessa transazione (un Create/Update/Delete che violerebbe la regola viene rifiutato o
rollbackato).

Le tre policy convivono in un solo posto:

- **ACL** (`check_access`): grant a livello di modello per gruppo, con semantica di unione
  (basta un gruppo che concede l'operazione); superuser sempre ammesso.
- **Record rule** (`record_rule_domain`): regole globali (senza gruppo) tutte richieste
  (AND), regole di gruppo applicabili in alternativa (OR), le due composte in AND. Una
  regola è un `Domain` tipizzato compilato in SQL parametrizzato, non una stringa valutata.
- **Multi-azienda** (`company_filter`): un chiamante non-superuser è **sempre**
  company-scoped (default-deny) sui modelli che hanno un campo `company_id` `Many2one`. Con
  un insieme consentito vede quelle aziende più le righe condivise (`company_id IS NULL`);
  con insieme vuoto vede solo le righe condivise. Solo `sudo` è non ristretto.

Gli errori del db (`DbError`) sono mappati a risposte HTTP coerenti: `AccessDenied` → 403,
`BadInput` → 400, `Conflict` (violazione unique/FK) → 409; un errore interno diventa un 500
opaco che non fa trapelare schema o SQL.

## Moduli, app e web

Tre piani distinti, con confini netti:

- **moduli** (`modules/*`) — crate Rust che definiscono modelli, estensioni, ACL, record
  rule, action, report, wizard e view. Dipendono **solo** dalla facciata `kigumi` e si
  auto-registrano nei registri `inventory`. Un modulo dichiara il proprio `ModuleManifest`
  e lo registra:

  ```rust
  // modules/base/src/lib.rs
  pub static MANIFEST: ModuleManifest = ModuleManifest {
      name: "base",
      version: "1.0.0",
      framework: ">=0.1, <0.2",
      depends: &[],
      summary: "Foundational models: currency, partner, company",
  };
  kigumi::register_module!(MANIFEST);
  ```

  Per scrivere un modulo proprio vedi [moduli-custom.md](./moduli-custom.md); per i moduli
  inclusi vedi [moduli.md](./moduli.md).

- **app** (`apps/*`) — gli eseguibili. `kigumi-cli` (binario `kigumi`) linka il framework
  e i moduli desiderati: linkare un modulo è ciò che fa entrare le sue registrazioni
  `inventory` nel binario. La CLI espone `kigumi serve` (migra catalogo + auth, fa
  bootstrap dell'admin da env, poi serve l'API sicura), `kigumi migrate` (migra tutti i
  moduli linkati + lo schema auth, poi esce) e i sottocomandi `kigumi config`,
  `kigumi user`, `kigumi acl`, `kigumi rule`, `kigumi module`, `kigumi version`. I
  moduli resi disponibili sono solo quelli il cui crate è linkato nel binario.

  ```toml
  # apps/kigumi-cli/Cargo.toml — i moduli linkati si auto-registrano nel catalogo
  kigumi-mod-base = { path = "../../modules/base" }
  kigumi-mod-mail = { path = "../../modules/mail" }
  kigumi-mod-sales = { path = "../../modules/sales" }
  kigumi-mod-account = { path = "../../modules/account" }
  kigumi-mod-stock = { path = "../../modules/stock" }
  ```

- **web** (`web/`) — il frontend (Vite/TypeScript), separato dal workspace Rust. È un
  consumatore del contratto-UI e dello schema OpenAPI generati dal server: non conosce lo
  schema a priori, lo legge come dato. Essendo Kigumi headless, il web è uno dei tanti
  client possibili (al pari di un SDK generato).

La separazione catalogo (compile time) vs set installato (runtime, per database) è
deliberata: tutti i moduli disponibili sono crate linkati, risolti e type-checked insieme;
quali moduli siano *attivi* per un'istanza è un dato a runtime (gestito da
`kigumi module install` / `kigumi module uninstall`), non una ricompilazione.

## Il modello di versionamento

Il framework usa **SemVer puro** (Cargo-native). `FRAMEWORK_VERSION` è la versione del
workspace, esposta dal core:

```rust
// crates/kigumi-core/src/lib.rs
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");
```

Ogni modulo ha la **propria** versione SemVer, indipendente dal framework, e dichiara nel
manifest (`crates/kigumi-core/src/manifest.rs`):

```rust
pub struct ModuleManifest {
    pub name: &'static str,
    pub version: &'static str,        // SemVer del modulo, es. "1.0.0"
    pub framework: &'static str,      // range di compatibilità col framework, es. ">=0.1, <0.2"
    pub depends: &'static [ModuleDep],// dipendenze su altri moduli, con range SemVer
    pub summary: &'static str,
}
```

Una dipendenza tra moduli è un `ModuleDep` con un range SemVer:

```rust
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,            // range SemVer, es. "^1.0"
}
```

Due meccanismi rendono il versionamento **verificabile**:

- **Range di compatibilità col framework** — `check_compat` verifica che la versione del
  framework rientri nel range `framework` dichiarato dal modulo. Un modulo fuori range è un
  errore, non un crash a runtime.
- **Versioni per-modulo con range sulle dipendenze** — ogni `ModuleDep` porta un range
  SemVer (`req`, es. `"^1.0"`). `resolve_module_set` (funzione pura sul set esplicito)
  verifica compat col framework, esistenza di ogni dipendenza con una versione che
  soddisfa il range, assenza di duplicati, self-dipendenze e cicli, restituendo i moduli in
  **ordine topologico**. `resolve_modules` è il wrapper sottile che alimenta questa
  funzione con il catalogo `inventory`.

Gli errori sono dedicati: `Incompatible`, `MissingDependency`, `DependencyConflict`,
`DuplicateModule`, `SelfDependency`, `DependencyCycle` (con i soli membri reali del ciclo).

### Policy sulle pre-release

Una build pre-release (es. `0.1.5-rc.1`) è trattata come la sua release line (`0.1.5`)
quando si confrontano i range, tramite `release_of`. Senza questa policy le regole
Cargo/SemVer rifiuterebbero ogni pre-release in-range, facendo fallire ogni install durante
le build RC/dev del framework. La boundary resta corretta: `0.2.0-rc.1` → `0.2.0`, ancora
fuori da `<0.2`.

### Migrazioni versionate

Il motore di migrazioni (`crates/kigumi-db/src/migration.rs`) è dichiarativo e
versionato: lo stato vive in `kigumi_module` (versione corrente) e `kigumi_migration`
(una riga per versione applicata). Ogni install/upgrade è **atomico** (singola
transazione), **serializzato** (`pg_advisory_xact_lock` per modulo) e **idempotente**
(ri-eseguire alla stessa versione è un no-op). Lo schema è generato dal `ResolvedModel`
(via `to_ddl`), non scritto a mano. Per il modello completo vedi
[`VERSIONING.md`](../../VERSIONING.md).

---

Vedi anche: [installazione.md](./installazione.md) per il setup,
[configurazione.md](./configurazione.md) per `kigumi.toml` e le variabili d'ambiente,
[api.md](./api.md) per gli endpoint REST e [sicurezza.md](./sicurezza.md) per ACL, record
rule e multi-azienda in dettaglio.
