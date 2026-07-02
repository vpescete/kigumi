# Moduli custom

Questa pagina è la guida completa alla scrittura di un modulo custom per Kigumi. Un modulo è un crate Rust che dichiara modelli, ACL, record rule, viste, compute, vincoli, azioni, report e wizard tramite macro e registri a compile time: tutto si auto-registra nel catalogo via `inventory`, e il binario che lo collega (`apps/kigumi-cli`) lo raccoglie e lo serve senza cablaggi manuali. Si parte dal crate, si arriva a un'API REST generata e a un test di integrazione. Per il quadro generale vedi [architettura.md](architettura.md) e [moduli.md](moduli.md); per installazione e configurazione [installazione.md](installazione.md) e [configurazione.md](configurazione.md); per la sicurezza [sicurezza.md](sicurezza.md); per le rotte [api.md](api.md).

---

## 1. Setup del crate

Un modulo vive in `modules/NAME/` ed è un normale crate Rust. La sua unica dipendenza obbligatoria è la facade `kigumi`, più ogni modulo da cui dipende (per riutilizzarne i modelli come target di relazione). Esempio reale, `modules/stock/Cargo.toml`:

```toml
[package]
name = "kigumi-mod-stock"
description = "Kigumi stock module: inventory — locations, quants, pickings and moves"
# MODULE version, independent of the framework (see docs/VERSIONING.md).
version = "1.0.0"
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
kigumi = { workspace = true }
# Exact-quantity arithmetic in the quant/move math.
rust_decimal = "1"
# Depends on base (company), sales (product.product), and mail (pickings carry a chatter thread).
kigumi-mod-base = { path = "../base", version = "1.0.0" }
kigumi-mod-sales = { path = "../sales", version = "1.0.0" }
kigumi-mod-mail = { path = "../mail", version = "1.0.0" }

[dev-dependencies]
kigumi-db = { workspace = true }
kigumi-mod-sales = { path = "../sales", version = "1.0.0" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

Note importanti:

- Il `version` del package è la **versione del modulo**, indipendente da quella del framework (SemVer per modulo). La versione del framework è condivisa da tutti i crate core (`0.1.0` nel workspace).
- Le dipendenze Cargo verso altri moduli (`kigumi-mod-base`, ...) devono rispecchiare il `depends` del manifest del modulo. Allineare le due liste è intenzionale: una dipendenza dichiarata nel manifest ma non collegata come crate Cargo non sarebbe presente in `inventory`.
- `rust_decimal` serve solo se il modulo fa aritmetica esatta (denaro, quantità); `serde_json` solo se ha report o codice che legge il record JSON.

### Collegare il crate al binario

Il modulo si auto-registra solo se il suo crate viene **linkato** nel binario finale. Si fa in due passi in `apps/kigumi-cli`.

Prima si aggiunge la dipendenza in `apps/kigumi-cli/Cargo.toml`:

```toml
# Linked so their models/ACLs/rules self-register into the catalog (inventory).
kigumi-mod-base = { path = "../../modules/base" }
kigumi-mod-mail = { path = "../../modules/mail" }
kigumi-mod-sales = { path = "../../modules/sales" }
kigumi-mod-account = { path = "../../modules/account" }
kigumi-mod-stock = { path = "../../modules/stock" }
```

Poi si referenzia un simbolo del crate dentro `link_modules()` in `apps/kigumi-cli/src/main.rs`, così il linker non scarta il crate (le sue registrazioni `inventory` sarebbero altrimenti assenti):

```rust
/// Forces the module crates to link so their `inventory` registrations are present in this binary.
fn link_modules() {
    let _ = (
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_account::MANIFEST,
        &kigumi_mod_stock::MANIFEST,
    );
}
```

`run()` chiama `link_modules()` come prima riga, prima di qualunque comando. Senza il riferimento al `MANIFEST`, modelli, ACL e rule del modulo non comparirebbero nel catalogo.

### Il prelude

Ogni `lib.rs` di modulo inizia con un'unica import:

```rust
use kigumi::prelude::*;
```

Il prelude (`crates/kigumi/src/lib.rs`) ri-esporta tutto il necessario: i tipi del metamodello (`FieldDef`, `FieldKind`, `Model`, `ModelDescriptor`, `ResolvedModel`), il manifest (`ModuleManifest`, `ModuleDep`), la sicurezza (`Acl`, `Ctx`, `Operation`, `RecordRule`, `RuleDomain`), il dominio (`Domain`, `Value`, `Operator`, `Condition`, ...), compute (`ComputeInput`, `ComputeFn`, `Children`), vincoli (`ConstraintFn`), azioni (`ActionInput`, `ActionOutcome`, `ActionFn`), report (`ReportFn`), wizard (`WizardContext`, `WizardDefaultGet`), viste (`FormView`, `FieldGroup`, `FieldSlot`, `NotebookPage`), la costante `FRAMEWORK_VERSION`, le macro `extend`/`model`, e da `kigumi_schema` `to_ddl`, `to_ui_contract`, `openapi`, `FieldRule`, `UiRule`. Le macro `register_*!` sono macro a livello di crate (`kigumi::register_acls!`, ...), richiamate con il prefisso `kigumi::`.

---

## 2. Dichiarare un modello

Un modello è una `struct` annotata con `#[model(name = "...", table = "...")]`. La macro `#[model]` (`crates/kigumi-macros/src/lib.rs`) **sostituisce** la struct con un tipo marker (`pub struct StockLocation;`), genera `impl Model` con un `ModelDescriptor` statico, e auto-registra il modello nel catalogo via `inventory::submit!`. I "tipi" dei campi (`Text`, `Many2one`, ...) sono parole chiave del DSL mappate su `FieldKind`, non veri tipi Rust.

```rust
#[model(name = "stock.location", table = "stock_location")]
pub struct StockLocation {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Type", required, default = "internal", selection = "internal:Internal,supplier:Vendor,customer:Customer,inventory:Inventory Loss,transit:Transit")]
    usage: Selection,

    #[field(label = "Parent Location", target = "stock.location")]
    parent_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}
```

Argomenti di `#[model(...)]`:

| Argomento | Obbligatorio | Significato |
|-----------|--------------|-------------|
| `name` | sì | Nome logico del modello (es. `"stock.location"`). |
| `table` | no | Tabella SQL. Se omesso, deriva da `name` sostituendo `.` con `_`. |
| `inherits` + `via` | no (insieme) | Inheritance per delegazione: il modello espone i campi scalari memorizzati del genitore attraverso la FK `via`. Vanno dichiarati entrambi o nessuno. Emette una `InheritsRegistration`. |

### Tipi di campo (FieldKind)

Il "tipo" della struct seleziona la variante `FieldKind` (`crates/kigumi-core/src/metamodel.rs`). Gli alias riconosciuti dalla macro sono esattamente questi; un alias diverso è un errore di compilazione.

| Alias DSL | FieldKind | Note / attributi richiesti |
|-----------|-----------|----------------------------|
| `Text` | `Text` | Testo semplice. |
| `Html` | `Html` | Rich text (`text`); sanitizzato a ogni scrittura (allowlist), widget `html`. |
| `Image` | `Image` | FK `bigint` verso `ir.attachment` (i byte stanno nel blob store). Letto/scritto come id dell'attachment. |
| `Integer` | `Integer` | Intero. |
| `Float` | `Float` | Virgola mobile non esatta (`double precision`): quantità, pesi, fattori, tassi. |
| `Decimal` | `Decimal { currency_field }` | Decimale esatto (`NUMERIC`). `currency = "campo"` lo rende monetario (widget `monetary`). |
| `Bool` | `Bool` | Booleano. |
| `Date` | `Date` | Data senza ora (`date`). |
| `Datetime` | `Datetime` | Timestamp con timezone (`timestamptz`). |
| `Selection` | `Selection(&[(k, label)])` | Richiede `selection = "k:Label,..."`. |
| `Many2one` | `Many2one { target }` | Relazione N→1, genera una colonna FK. Richiede `target = "model.name"`. |
| `One2many` | `One2many { target, inverse }` | Relazione 1→N, nessuna colonna (vive sull'inverso). Richiede `target` e `inverse`. |
| `Many2many` | `Many2many { target, relation, column, target_column }` | Relazione N↔N via tabella di giunzione. Nessuna colonna sul modello. Richiede tutti e quattro. |

### Riferimento completo degli attributi `#[field(...)]`

Tutti gli attributi sono parsati in `build_field` (e nelle funzioni di submission ausiliarie) di `crates/kigumi-macros/src/lib.rs`. Riempiono il `FieldDef` (`metamodel.rs`) oppure emettono una registrazione laterale.

| Attributo | Forma | Effetto |
|-----------|-------|---------|
| `label` | `label = "..."` | Etichetta UI. Default: il nome del campo. |
| `required` | flag | `FieldDef.required = true` (NOT NULL). |
| `default` | `default = "..."` | Valore di default come stringa, parsato per tipo, applicato in create quando il campo è non impostato. |
| `selection` | `selection = "k:Label,..."` | Coppie chiave:etichetta per un campo `Selection`. |
| `target` | `target = "model.name"` | Modello bersaglio di `Many2one` / `One2many` / `Many2many`. |
| `inverse` | `inverse = "campo"` | Campo inverso di un `One2many` (il `Many2one` sul figlio). |
| `relation` / `column` / `target_column` | stringhe | Tabella di giunzione e le due colonne di un `Many2many` (`column` → questo modello, `target_column` → il target). |
| `related` | `related = "path"` | Campo `related`: specchio non memorizzato e in sola lettura del valore raggiunto seguendo un percorso relazionale (es. `order_id.partner_id`). Emette una `RelatedRegistration`; non genera colonna (`stored = false`). |
| `compute` | `compute = "nome_fn"` | Nome della funzione di compute registrata per il campo. |
| `depends` | `depends = "a,b,line_ids.x"` | Dipendenze del compute (CSV). Verificate da `validate_depends`: un campo dipendente inesistente è un errore. Un compute on-read (non memorizzato) non può dipendere da un percorso relazionale dotted. |
| `store` | flag | Memorizza un campo computato (colonna materializzata in scrittura). Senza `store`, un campo con `compute` è on-read (nessuna colonna, ricalcolato a ogni lettura). |
| `tracked` | flag | Traccia le modifiche del campo nel chatter (richiede modello mailed). Emette una `TrackedFieldRegistration`. |
| `groups` | `groups = "a,b"` | Sicurezza di campo (D6): lettura E scrittura del campo richiedono l'appartenenza ad almeno uno dei gruppi. Emette una `FieldGroupRegistration`. Il superuser bypassa. |
| `currency` | `currency = "campo"` | Solo per `Decimal`: campo valuta collegato (widget `monetary`). |
| `unique` | flag | Genera un vincolo `UNIQUE` su colonna singola nel DDL. |
| `check` | `check = "espr SQL"` | Espressione `CHECK` SQL grezza (fidata, a compile time) → vincolo `CHECK` di colonna. |

Regola di memorizzazione (`stored`) calcolata dalla macro:

- `One2many`, `Many2many` e i campi con `related` non sono mai memorizzati (nessuna colonna).
- Un campo con `compute` è memorizzato solo se ha anche `store`.
- Tutti gli altri sono memorizzati.

Esempi reali (da `modules/sales/src/lib.rs` e `modules/base/src/lib.rs`):

```rust
// Decimale monetario, aggregato esatto sui figli, memorizzato:
#[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_total", currency = "currency_id", store)]
amount_total: Decimal,

// Campo related (specchio read-only): il cliente dell'ordine, da order_id.partner_id.
#[field(label = "Customer", target = "res.partner", related = "order_id.partner_id")]
order_partner_id: Many2one,

// Sicurezza di campo: il costo è manager-only (lettura e scrittura).
#[field(label = "Cost", default = "0", groups = "sales.manager")]
purchase_price: Decimal,

// unique + check su res.currency:
#[field(label = "Code", required, unique)]
code: Text,
#[field(label = "Decimal Places", default = "2", check = "decimal_places >= 0")]
decimal_places: Integer,
```

### Estendere un modello con `#[extend]`

`#[extend("model.name")]` aggiunge campi a un modello definito altrove, senza toccarne la base. L'estensione si auto-registra come `FieldExtension` e viene fusa da `resolve_registered` con controllo di conflitti (un campo già presente è un errore). Dal modulo `sales`:

```rust
/// `sale_margin` extension: adds `margin` via `#[extend]`, WITHOUT touching the base.
#[extend("sale.order")]
pub struct SaleMargin {
    #[field(label = "Margin", compute = "compute_margin", depends = "line_ids.margin", currency = "currency_id", store)]
    margin: Decimal,
}
```

I campi accettano lo stesso identico set di attributi `#[field(...)]` del `#[model]`. Questo è il meccanismo con cui, ad esempio, il modulo account può "adottare" `account.tax` (stesso nome, nessuna migrazione) aggiungendo campi.

---

## 3. I registri

Tutti i registri sono macro a livello di crate richiamate al **top level** del modulo. Ognuna emette una `inventory::submit!` con una struct specifica.

### `register_module!` — il manifest

Ogni modulo dichiara un `ModuleManifest` statico e lo registra. Da `modules/stock/src/lib.rs`:

```rust
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "stock",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[
        ModuleDep { name: "base", req: "^1.0" },
        ModuleDep { name: "sales", req: "^1.0" },
        ModuleDep { name: "mail", req: "^1.0" },
    ],
    summary: "Inventory — locations, quants, pickings and moves",
};
kigumi::register_module!(MANIFEST);
```

`ModuleManifest` (`crates/kigumi-core/src/manifest.rs`) ha i campi `name`, `version` (SemVer del modulo), `framework` (range di compatibilità con il framework, es. `">=0.1, <0.2"`), `depends` (slice di `ModuleDep { name, req }` con range SemVer verificati) e `summary`. Il resolver (`resolve_module_set`) valida compatibilità framework, range delle dipendenze, assenza di duplicati, di auto-dipendenze e di cicli, e restituisce i moduli in ordine topologico.

### `register_acls!` — la struct `Acl`

ACL a livello di modello (`crates/kigumi-core/src/security.rs`). Una `&'static [Acl]` registrata; il server le raccoglie via `registered_acls()`.

```rust
pub struct Acl {
    pub model: &'static str,
    pub group: &'static str,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}
```

L'accesso è concesso se **uno qualunque** dei gruppi dell'utente lo concede (semantica di unione). Il superuser è sempre ammesso. Da `modules/stock/src/lib.rs`:

```rust
pub static ACLS: &[Acl] = &[
    Acl { model: "stock.location", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.location", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.picking", group: "stock.user", read: true, write: true, create: true, delete: false },
    Acl { model: "stock.move", group: "stock.user", read: true, write: true, create: true, delete: true },
    // ...
];
kigumi::register_acls!(ACLS);
```

### `register_rules!` — `RecordRule`, `RuleDomain` e la DSL `Domain`

Le record rule sono regole a livello di riga. Il dominio non è una stringa valutata a runtime, ma dato tipato compilato in SQL parametrizzato.

```rust
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],   // vuoto = globale (vale per tutti)
    pub ops: &'static [Operation],         // Read / Write / Create / Delete
    pub domain: RuleDomain,
}

pub enum RuleDomain {
    Static(fn() -> Domain),   // regola statica di modulo (un thunk: Domain non è const-construibile)
    Owned(Domain),            // regola caricata da DB a runtime
}
```

Semantica di combinazione (`record_rule_domain`): le regole globali (senza gruppo) sono tutte richieste → AND; le regole dei gruppi applicabili all'utente sono alternative → OR; i due insiemi vengono poi messi in AND. Il superuser non è soggetto ad alcuna restrizione.

La DSL `Domain` (`crates/kigumi-core/src/domain.rs`) si costruisce in modo fluente:

```rust
Domain::field("state").ne("done")                       // state <> 'done'
Domain::field("amount_total").lt(10_000_i64)            // amount_total < 10000
Domain::field("order_id.state").ne("done")             // percorso dotted → subquery
Domain::field("partner_id").is_not_null()
Domain::field("state").in_(["draft", "sale"])

// combinatori:
a.and(b)   a.or(b)   a.not()
```

Operatori disponibili sul `FieldBuilder`: `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `like`, `ilike`, `is_null`, `is_not_null`, `in_`, `not_in`. Un percorso dotted attraverso un `Many2one`/`One2many` diventa una subquery (NULL-safe), così le rule possono attraversare relazioni. Domini non validi (campo inesistente, tipo incompatibile, operatore non adatto al tipo, percorso non relazionale) sono rifiutati alla compilazione del dominio, non in produzione.

Esempio reale da `stock` (i move di un trasferimento "done" sono congelati):

```rust
fn move_picking_not_done() -> Domain {
    Domain::field("picking_id.state").ne("done")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(move_picking_not_done) },
];
kigumi::register_rules!(RECORD_RULES);
```

### `register_action!` — funzioni d'azione e `ActionOutcome`

Un'azione è una transizione di stato nominata su un modello, eseguibile via `POST /api/<model>/<id>/action/<name>`. La firma:

```rust
pub type ActionFn = fn(&ActionInput) -> Result<ActionOutcome, String>;
```

`ActionInput` (`crates/kigumi-core/src/action.rs`) è la vista in sola lettura del record corrente, con accessor tipati: `str(field)`, `int(field)`, `decimal(field)`, `bool(field)`, `get(field)`. La guardia ("solo se draft") vive nel corpo e ritorna `Err(messaggio)` per rifiutare. `ActionOutcome` raccoglie gli aggiornamenti di campo (`set`) più una direttiva opzionale `assign_sequence(field, code)`, risolta dal layer di persistenza (per numerazione senza buchi). Da `modules/sales/src/lib.rs`:

```rust
fn confirm_order(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("sale".to_string()))
            .set("invoice_status", Value::Str("to_invoice".to_string()))
            .assign_sequence("name", "SO")),
        s => Err(format!("can only confirm a draft order (state is '{s}')")),
    }
}
kigumi::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);
```

L'ultimo argomento è la slice di gruppi che possono eseguire l'azione (oltre alla ACL Write + record rule del modello); `&[]` non restringe oltre.

### `register_report!`

Un report è una `fn(&serde_json::Value) -> String` pura che rende un record (con i figli One2many già inlinati) in un documento HTML, esposto a `GET /api/<model>/<id>/report/<name>` e protetto dall'accesso in lettura al record.

```rust
pub type ReportFn = fn(&serde_json::Value) -> String;
```

```rust
kigumi::register_report!("sale.order", "quotation", "Quotation", render_quotation);
```

Gli argomenti sono `model`, `name` (segmento URL), `title` (etichetta umana / nome file) e la funzione di render. Il contenuto memorizzato è non fidato: va sempre escapato (la `render_quotation` reale usa un helper `esc` per evitare XSS persistente).

### `register_wizard!` e `register_transient!` — modelli transient e `default_get`

Un wizard è un modello **transient** (uno scratchpad con righe effimere, raccolte da un cron orario per età) collegato a una funzione `default_get`. Il modello transient deve dichiarare un campo `create_date: Datetime` nullable (la migrazione gli dà un `DEFAULT now()`). Da `modules/sales/src/lib.rs`:

```rust
#[model(name = "sale.order.discount", table = "sale_order_discount")]
pub struct SaleOrderDiscount {
    #[field(label = "Order", required, target = "sale.order")]
    order_id: Many2one,
    #[field(label = "Discount %", default = "0")]
    discount: Decimal,
    // GC timestamp: migration gives this a DEFAULT now(); the transient cron reclaims aged rows.
    #[field(label = "Created")]
    create_date: Datetime,
}
kigumi::register_transient!("sale.order.discount");
kigumi::register_wizard!("sale.order.discount", default_get_discount);

/// default_get: seed `order_id` from the open context's active record.
fn default_get_discount(ctx: &WizardContext) -> Vec<(&'static str, Value)> {
    match ctx.active_id {
        Some(id) => vec![("order_id", Value::Int(id))],
        None => vec![],
    }
}
```

`WizardDefaultGet` ha firma `fn(&WizardContext) -> Vec<(&'static str, Value)>`; `WizardContext` (`crates/kigumi-core/src/wizard.rs`) porta `active_model`, `active_id`, `active_ids`. È puro in v1 (nessun accesso DB). Il wizard si apre via `POST /api/<model>/open`, che calcola i default, crea la riga scratchpad sotto il chiamante (ACL di create normale) e la restituisce. La logica di "apply" è un metodo di servizio dedicato per-wizard più un endpoint (es. `apply_discount` → `POST /api/sale.order.discount/<id>/apply_discount`), **non** parte della registrazione del wizard.

### `register_mailed!`

Una riga, nessun mixin: il modello acquisisce un thread di messaggi, follower e attività via il link polimorfico `(res_model, res_id)`, e il framework pulisce il thread alla cancellazione del record. È la precondizione perché `#[field(tracked)]` registri le modifiche nel chatter.

```rust
kigumi::register_mailed!("stock.picking");
```

### `register_view!` — `FormView`, `FieldGroup`, `FieldSlot`, `NotebookPage`

Una vista form (`crates/kigumi-core/src/view.rs`) è dato statico emesso nel contratto-UI, così il frontend rende una vista reale invece di scaricare i campi in ordine di dichiarazione. Le strutture:

```rust
pub struct FieldSlot   { pub name: &'static str, pub full: bool }            // full = occupa entrambe le colonne
pub struct FieldGroup  { pub title: Option<&'static str>, pub fields: &'static [FieldSlot] }
pub struct NotebookPage{ pub title: &'static str, pub fields: &'static [&'static str] }
pub struct FormView    { pub model: &'static str, pub groups: &'static [FieldGroup], pub pages: &'static [NotebookPage] }
```

La macro prende `model`, la slice di `FieldGroup` e la slice di `NotebookPage`, ed emette un `FormView`. Da `modules/base/src/lib.rs`:

```rust
kigumi::register_view!(
    "res.partner",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "is_company", full: false },
                FieldSlot { name: "parent_id", full: false },
                FieldSlot { name: "active", full: false },
            ],
        },
        FieldGroup {
            title: Some("Contact"),
            fields: &[FieldSlot { name: "email", full: false }, FieldSlot { name: "phone", full: false }],
        },
    ],
    &[]   // nessuna notebook page
);
```

Per una vista con notebook (relazione One2many in una tab), da `sales`:

```rust
&[NotebookPage { title: "Order lines", fields: &["line_ids"] }]
```

---

## 4. Funzioni di compute

Un compute è una `fn(&ComputeInput) -> Value` pura registrata per nome con `register_compute!`. L'engine (`crates/kigumi-core/src/compute.rs`) riempie ogni campo computato memorizzato (in scrittura, `compute_stored`) o on-read (a ogni lettura, `compute_on_read`) la cui funzione è registrata.

`ComputeInput` è la vista in sola lettura del record (i suoi campi + i figli One2many). Accessor scalari: `int`, `float`, `str`, `bool`, `decimal`, `get`. Accessor di aggregazione sui figli: `children(o2m)`, `count(o2m)`, `sum_float(o2m, child_field)`, `sum_decimal(o2m, child_field)` (somma esatta, senza arrotondamento f64). `Value` è l'enum dei valori (`Str`, `Int`, `Float`, `Decimal`, `Bool`, `Null`, `List`).

Compute same-record (entrambi gli input sul record) e aggregato (sui figli), da `sales`:

```rust
/// A line's subtotal = discounted net (qty × unit price × (1 - discount%)).
fn compute_line_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(line_net(i))
}
kigumi::register_compute!("compute_line_subtotal", compute_line_subtotal);

/// amount_total of an order = exact sum of its lines' taxed totals.
fn compute_amount(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_total"))
}
kigumi::register_compute!("compute_amount", compute_amount);
```

Regole chiave:

- Un compute memorizzato (campo con `compute` + `store`) è valutato in scrittura, può aggregare sui figli e può dichiarare `depends` con percorsi relazionali dotted.
- Un compute on-read (campo con `compute` senza `store`) è valutato a ogni lettura, è same-record (i figli non sono caricati) e **non** può avere `depends` dotted (`validate_depends` lo rifiuta).
- I `depends` sono verificati: una dipendenza verso un campo inesistente è un errore di risoluzione.

---

## 5. Vincoli in transazione (`constrains`)

Un vincolo cross-record (`crates/kigumi-core/src/constraints.rs`) gira **dentro la transazione di scrittura**, dopo che il record e i suoi figli One2many sono stati scritti e ri-letti, e rifiuta la scrittura (errore tipato, rollback) se l'invariante è violata. A differenza di un `CHECK` SQL (single-row), legge l'header insieme ai figli tramite lo stesso `ComputeInput` dell'engine di compute, quindi esprime invarianti che attraversano un header e le sue righe.

La firma e il limite:

```rust
pub type ConstraintFn = fn(&ComputeInput) -> Result<(), String>;
```

**Limite importante: un `ConstraintFn` non ha accesso al DB.** Legge solo i valori già presenti nel `ComputeInput` (il record e i figli scritti), non può fare query. Invarianti che richiedono di leggere altri record (es. la company di un account riferito via FK) non sono esprimibili qui e vanno chiuse con una record rule o una validazione FK company-aware.

Si registra con `register_constraint!(model, &[campi_trigger], func)`. I campi trigger sono i campi scritti (e i nomi dei campi One2many) che guidano l'invariante; una lista vuota fa girare il vincolo a ogni scrittura. Su create gira sempre. Esempio canonico (entry contabile bilanciata) da `modules/account/src/lib.rs`:

```rust
fn check_balanced(m: &ComputeInput) -> Result<(), String> {
    let debit: Decimal = m.sum_decimal("line_ids", "debit");
    let credit: Decimal = m.sum_decimal("line_ids", "credit");
    if debit != credit {
        return Err(format!("unbalanced journal entry: total debit {debit} != total credit {credit}"));
    }
    Ok(())
}
kigumi::register_constraint!("account.move", &["line_ids"], check_balanced);
```

In v1 i vincoli girano sul modello top-level scritto: un vincolo su un figlio scritto attraverso i comandi nested One2many del genitore, o su un genitore in inheritance per delegazione (`inherits`/`via`), non è valutato.

---

## 6. Operazioni cross-record: metodo di servizio + endpoint dedicato

Azioni, compute e vincoli coprono le transizioni single-record e le invarianti header+righe. Per operazioni che toccano **più record** in una transazione (creare documenti collegati, spostare giacenza, postare scritture) il pattern è diverso: un **metodo di servizio** sul `Db`, autorizzato esplicitamente con `check_access`, che poi esegue l'effetto elevato (`ctx.sudo()`), più una **rotta axum dedicata** che autentica e lo invoca. Questo non passa per la macro `register_action!`.

Il caso reale è `validate_picking` / `create_delivery` (`crates/kigumi-db/src/lib.rs`).

### Il metodo di servizio

`validate_picking` valida un trasferimento draft: setta i suoi move done, aggiorna i quant e numera il documento, tutto in una transazione. Estratto della struttura (gate di accesso → lettura sicura → effetto in transazione):

```rust
pub async fn validate_picking(
    &self,
    ctx: &Ctx,
    acls: &[Acl],
    rules: &[RecordRule],
    picking_id: i64,
) -> Result<String, DbError> {
    let picking_model = resolve_registered("stock.picking").map_err(DbError::BadInput)?;

    // 1) Gate: il chiamante deve avere WRITE su stock.picking.
    if !check_access(Operation::Write, picking_model.name, ctx, acls) {
        return Err(DbError::AccessDenied { model: picking_model.name.to_string(), operation: "validate" });
    }
    // 2) Lettura sicura del record sotto ACL + record rule del chiamante.
    let picking = self
        .find_one_secured(&picking_model, ctx, acls, rules, picking_id)
        .await?
        .ok_or_else(|| DbError::BadInput("transfer not found or not permitted".to_string()))?;
    // 3) Guardia di stato e poi l'effetto in transazione (quant upsert, move → done, numerazione).
    // ... (begin tx, FOR UPDATE compare-and-set su state='draft', upsert quant, commit) ...
}
```

`create_delivery` mostra il pattern dell'effetto elevato. Crea una consegna draft da un ordine confermato: il gate è sulla WRITE dell'ordine del chiamante, ma la risoluzione delle location e la creazione di picking + move avvengono **elevate** (`ctx.sudo()`), così un commerciale non deve anche avere i diritti di create sullo stock:

```rust
pub async fn create_delivery(
    &self, ctx: &Ctx, acls: &[Acl], rules: &[RecordRule], order_id: i64,
) -> Result<i64, DbError> {
    self.create_order_picking(ctx, acls, rules, "sale.order", order_id,
        "create_delivery", "sale", "delivery", "internal", "customer").await
}
```

dove `create_order_picking` fa, semplificato:

```rust
if !check_access(Operation::Write, order_model.name, ctx, acls) {
    return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation });
}
let order = self.find_one_secured(&order_model, ctx, acls, rules, order_id).await?
    .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
// ... guardia di stato ...
// L'effetto di sistema (risolvere endpoint + creare picking/move) gira elevato:
let elevated = ctx.sudo();
let picking_id = self.insert_secured(&picking_model, &elevated, &[], &[], payload.as_object().unwrap()).await?;
// ... insert dei move elevati ...
```

### La rotta axum

Ogni metodo di servizio ha un handler dedicato in `crates/kigumi-server/src/lib.rs`, registrato in `router_with_data_rasterized`:

```rust
.route("/api/:name/:id/validate", post(validate_picking_handler))
.route("/api/:name/:id/create_delivery", post(create_delivery_handler))
.route("/api/:name/:id/create_receipt", post(create_receipt_handler))
```

L'handler vincola il modello (v1: pinned), autentica il `Ctx` dal bearer, invoca il metodo e mappa l'errore:

```rust
async fn create_delivery_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    if name != "sale.order" {
        return (StatusCode::BAD_REQUEST, "create_delivery is only valid on sale.order").into_response();
    }
    if let Err(r) = resolve_model(&state, &name) { return r; }
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) { Ok(c) => c, Err(r) => return r };
    match backend.db.create_delivery(&ctx, backend.acls, backend.rules, id).await {
        Ok(picking) => json_status(StatusCode::CREATED, serde_json::json!({ "picking": picking }).to_string()),
        Err(e) => write_error("create_delivery", e),
    }
}
```

In sintesi: l'autorizzazione vive nel layer db (`check_access`), l'effetto di sistema gira elevato dopo il gate, e l'handler si limita ad autenticare e a dare forma alla risposta.

---

## 7. Esempio end-to-end: un piccolo modulo `library`

Mettiamo insieme tutto con un modulo nuovo e minimale: un catalogo di libri.

### 7.1 `modules/library/Cargo.toml`

```toml
[package]
name = "kigumi-mod-library"
description = "Kigumi library module: a tiny book catalog"
version = "1.0.0"
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
kigumi = { workspace = true }
# Dipende da base per usare res.partner (l'autore) come target di relazione.
kigumi-mod-base = { path = "../base", version = "1.0.0" }

[dev-dependencies]
kigumi-db = { workspace = true }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

### 7.2 `modules/library/src/lib.rs`

```rust
//! Application module `library`: a tiny book catalog.
use kigumi::prelude::*;

pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "library",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^1.0" }],
    summary: "A tiny book catalog",
};
kigumi::register_module!(MANIFEST);

#[model(name = "library.book", table = "library_book")]
pub struct LibraryBook {
    #[field(label = "Title", required)]
    name: Text,

    #[field(label = "ISBN", unique)]
    isbn: Text,

    #[field(label = "Author", target = "res.partner")]
    author_id: Many2one,

    #[field(label = "Status", required, default = "available", selection = "available:Available,borrowed:Borrowed")]
    state: Selection,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Access control: members read the catalog, librarians maintain it.
pub static ACLS: &[Acl] = &[
    Acl { model: "library.book", group: "library.member", read: true, write: false, create: false, delete: false },
    Acl { model: "library.book", group: "library.librarian", read: true, write: true, create: true, delete: true },
];
kigumi::register_acls!(ACLS);

/// Members never see borrowed books in the catalog list.
fn only_available() -> Domain {
    Domain::field("state").eq("available")
}
pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule {
        model: "library.book",
        groups: &["library.member"],
        ops: &[Operation::Read],
        domain: RuleDomain::Static(only_available),
    },
];
kigumi::register_rules!(RECORD_RULES);

/// `borrow`: an available book becomes borrowed.
fn borrow_book(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "available" => Ok(ActionOutcome::new().set("state", Value::Str("borrowed".to_string()))),
        s => Err(format!("only an available book can be borrowed (state is '{s}')")),
    }
}
kigumi::register_action!("library.book", "borrow", borrow_book, &["library.librarian"]);

/// Form layout.
kigumi::register_view!(
    "library.book",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "name", full: true },
            FieldSlot { name: "isbn", full: false },
            FieldSlot { name: "author_id", full: false },
            FieldSlot { name: "state", full: false },
            FieldSlot { name: "active", full: false },
        ],
    }],
    &[]
);
```

### 7.3 Collegare il modulo

In `apps/kigumi-cli/Cargo.toml`:

```toml
kigumi-mod-library = { path = "../../modules/library" }
```

In `apps/kigumi-cli/src/main.rs`, dentro `link_modules()`:

```rust
let _ = (
    &kigumi_mod_base::MANIFEST,
    // ... gli altri ...
    &kigumi_mod_library::MANIFEST,
);
```

### 7.4 Installare il modulo

Su un database fresco la migrazione installa solo `base` (+ closure); gli altri moduli sono opt-in. Dopo aver compilato il binario `kigumi`:

```sh
# Migra le schema framework + base (installazione iniziale)
kigumi migrate

# Installa library e la sua dependency closure (deps prima), poi migra le tabelle
kigumi module install library

# Verifica
kigumi module list
```

`module install` chiama `module_closure(name)` (nome + dipendenze transitive, deps prima), marca i moduli come installati e poi rilancia `migrate_installed` (idempotente) per creare le tabelle. Avvia poi il server (che serve solo i modelli dei moduli installati):

```sh
kigumi serve
```

### 7.5 Curl contro l'API generata

Tutte le rotte dati richiedono un bearer. Prima si fa login (`POST /auth/login` restituisce `access_token` / `refresh_token` / `token_type: "Bearer"` / `expires_in`). Il server ascolta per default su `127.0.0.1:8099` (`server.bind` in `kigumi.toml`):

```sh
TOKEN=$(curl -s http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$KIGUMI_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')

# Crea un libro (POST /api/:name → { "id": <n> } con 201)
curl -s http://127.0.0.1:8099/api/library.book \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"The Rust Programming Language","isbn":"9781718500457","state":"available"}'

# Lista (GET /api/:name) → envelope { data, total, limit, offset }
curl -s http://127.0.0.1:8099/api/library.book -H "Authorization: Bearer $TOKEN"

# Esegui l'azione borrow (POST /api/:name/:id/action/:action)
curl -s -X POST http://127.0.0.1:8099/api/library.book/1/action/borrow \
  -H "Authorization: Bearer $TOKEN"

# Recupera il contratto-UI della vista (GET /api/:name/view)
curl -s http://127.0.0.1:8099/api/library.book/view -H "Authorization: Bearer $TOKEN"
```

Le rotte CRUD generate sono `GET/POST /api/:name`, `GET/PATCH/DELETE /api/:name/:id`, l'azione `POST /api/:name/:id/action/:action`, il report `GET /api/:name/:id/report/:report`, l'apertura wizard `POST /api/:name/open`. Vedi [api.md](api.md) per l'elenco completo.

---

## 8. Test di integrazione di un modulo

Il pattern usato in `modules/stock/tests/` è: un test `#[tokio::test]` che si **salta** se `DATABASE_URL` non è impostato, collega i moduli, ricrea lo schema dal `migration_plan()` e opera sul `Db` con un `Ctx` superuser. Le dipendenze di test stanno in `[dev-dependencies]` (vedi `kigumi-db`, `tokio`, `serde_json` nel `Cargo.toml` del modulo).

Scheletro (da `modules/stock/tests/validate.rs`, ridotto):

```rust
use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

/// Forza il link dei moduli così le loro registrazioni inventory sono nel binario di test.
fn link() {
    let _ = (
        &kigumi_mod_stock::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
    );
}

#[tokio::test]
async fn validate_moves_stock_and_is_single_shot() {
    link();
    // Si salta senza DATABASE_URL (così la suite passa anche senza DB).
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    // Ricrea lo schema dal piano di migrazione (ordinato per FK), idempotente.
    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_m2m_relations(&t.model).await.unwrap(); }
    db.ensure_stock_indexes().await.unwrap();
    db.ensure_sequence_schema().await.unwrap();

    // Risolvi i modelli dal catalogo e inserisci dati con insert_secured.
    let picking = resolve_registered("stock.picking").unwrap();
    // ... let receipt = db.insert_secured(&picking, &su, &[], &[], v.as_object().unwrap()).await.unwrap();

    // Esegui il metodo di servizio e fai le asserzioni.
    let n1 = db.validate_picking(&su, &[], &[], receipt).await.unwrap();
    assert!(n1.starts_with("IN/"));

    // Pulizia.
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
```

Punti essenziali del pattern:

- `link()` referenzia i `MANIFEST` di ogni modulo coinvolto (incluse le dipendenze), altrimenti i loro modelli non sarebbero registrati nel binario di test.
- Il test ritorna senza fallire se `DATABASE_URL` non è presente, così `cargo test` resta verde anche su una macchina senza Postgres.
- `migration_plan()` dà i target ordinati per FK (`MigrationTarget { module, version, model }`); si dropano in ordine inverso e si creano in ordine diretto, poi le relazioni Many2many in un secondo passaggio.
- Si lavora con un `Ctx` superuser (`Ctx::new(0, vec![]).sudo()`) per saltare ACL/record rule nel setup, e si usano i metodi `*_secured` del `Db` (`insert_secured`, `find_one_secured`, `find_secured`, `count_secured`).

Per le verifiche non-DB (forma del descriptor, presenza degli attributi) basta un test unitario `#[cfg(test)]` nel `lib.rs` del modulo che chiama `resolve_registered("...")` e ispeziona i `FieldDef`, come negli esempi in `modules/sales/src/lib.rs`.
