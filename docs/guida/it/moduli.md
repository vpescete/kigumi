# Moduli

Kigumi è un framework ERP headless guidato da schema: ogni funzionalità di dominio è impacchettata in un **modulo**. Un modulo è una crate Rust che, tramite un `ModuleManifest` e una manciata di macro `register_*!`, dichiara i propri modelli, le ACL, le record rule, le azioni e i metodi di servizio nel registro a compile time. Questa pagina descrive il sistema dei moduli — il manifest, la dichiarazione delle dipendenze, la chiusura delle dipendenze, la semantica di install/uninstall, il registro di installazione e il controllo di compatibilità con il framework — e poi cataloga i moduli inclusi (`base`, `mail`, `sales`, `account`, `stock`) con i modelli che spediscono e le loro feature principali. Per scrivere un modulo proprio, vedi [moduli-custom.md](./moduli-custom.md).

## Il `ModuleManifest`

Ogni modulo dichiara un `ModuleManifest` statico — dati dichiarativi validati a build/install time. La struttura è definita in `crates/kigumi-core/src/manifest.rs`:

```rust
pub struct ModuleManifest {
    pub name: &'static str,
    /// SemVer of the module, e.g. "1.0.0".
    pub version: &'static str,
    /// Compatibility range with the framework, e.g. ">=0.1, <0.2".
    pub framework: &'static str,
    /// Dependencies on other modules, with version ranges.
    pub depends: &'static [ModuleDep],
    pub summary: &'static str,
}
```

| Campo | Tipo | Significato |
|-------|------|-------------|
| `name` | `&'static str` | Nome tecnico del modulo (es. `"sales"`). Deve essere unico nel catalogo. |
| `version` | `&'static str` | Versione SemVer del modulo, indipendente da quella del framework. |
| `framework` | `&'static str` | Range SemVer di compatibilità con il framework (es. `">=0.1, <0.2"`). |
| `depends` | `&'static [ModuleDep]` | Dipendenze da altri moduli, ognuna con il proprio range di versione. |
| `summary` | `&'static str` | Descrizione breve, mostrata da `kigumi module list`. |

Ogni dipendenza è un `ModuleDep`, ovvero il nome del modulo richiesto più un vincolo di versione SemVer:

```rust
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,
}
```

A differenza di una semplice lista di nomi, ogni dipendenza porta un **range di versione verificabile** (es. `^1.0`): la risoluzione controlla che il modulo dipeso sia presente *e* che la sua versione soddisfi il range, non solo che esista.

### `register_module!`

Il manifest da solo non è visibile al catalogo: il modulo deve registrarlo. Si fa con una riga, al livello top del modulo, subito dopo aver definito il `MANIFEST`:

```rust
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "base",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[],
    summary: "Foundational models: currency, partner, company",
};
kigumi::register_module!(MANIFEST);
```

La macro (definita in `crates/kigumi/src/lib.rs`) emette una `ModuleRegistration` nel registro a compile time tramite `inventory`, conservando anche il `module_path!()` della crate. Quel percorso è ciò che consente a `module_of(model)` di risalire dal modello al modulo che lo possiede — la base del gating per modulo installato in migrazione e in servizio.

Perché le registrazioni `inventory` siano presenti nel binario, la crate del modulo deve essere effettivamente linkata. Il binario `kigumi` lo forza in `apps/kigumi-cli/src/main.rs`:

```rust
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

Un modulo è **disponibile** quando la sua crate è linkata nel binario (compile time); diventa **installato** solo quando ha una riga nel registro di installazione (vedi sotto).

## Il controllo di compatibilità con il framework

Prima di qualunque risoluzione, ogni manifest viene confrontato con la versione del framework dalla funzione `check_compat` (in `crates/kigumi-core/src/manifest.rs`). La versione del framework è la costante `FRAMEWORK_VERSION`, derivata da `CARGO_PKG_VERSION` di `kigumi-core` (definita in `crates/kigumi-core/src/lib.rs`) — al momento `0.1.1`. Tutti i moduli inclusi dichiarano `framework = ">=0.1, <0.2"`, quindi sono compatibili con questa linea.

```rust
pub fn check_compat(
    manifest: &ModuleManifest,
    framework_version: &str,
) -> Result<(), ResolutionError> {
    let fw = Version::parse(framework_version)?;
    let _ = Version::parse(manifest.version)?;
    let req = VersionReq::parse(manifest.framework)?;
    if !req.matches(&release_of(&fw)) {
        return Err(ResolutionError::Incompatible {
            module: manifest.name.to_string(),
            needs: manifest.framework.to_string(),
            found: framework_version.to_string(),
        });
    }
    Ok(())
}
```

Una build pre-release (es. `0.1.5-rc.1`) viene trattata come la sua linea di rilascio (`0.1.5`) tramite `release_of`, così le RC/dev build dello stesso ciclo restano in range. Una pre-release della linea *successiva* (`0.2.0-rc.1`), invece, resta fuori range e fallisce.

### Risoluzione e ordinamento topologico

`resolve_module_set` prende uno slice di manifest più la versione del framework e restituisce i moduli in **ordine di dipendenza (topologico)** — le dipendenze prima dei dipendenti. Lungo il percorso verifica:

- compatibilità con il framework di ogni modulo (`check_compat`);
- che ogni dipendenza esista nel catalogo, altrimenti `MissingDependency`;
- che la versione della dipendenza soddisfi il range richiesto, altrimenti `DependencyConflict`;
- assenza di nomi duplicati (`DuplicateModule`) e di auto-dipendenze (`SelfDependency`);
- assenza di cicli — un ordinamento topologico alla Kahn; in caso di ciclo, `DependencyCycle` riporta **solo** i moduli effettivamente sul ciclo (la coda a valle viene rimossa).

L'ordinamento è deterministico: a parità di disponibilità (più moduli pronti contemporaneamente) i nomi sono processati in ordine alfabetico, perché la risoluzione li indicizza in una `BTreeMap`. È per questo che `account` precede `sales` nell'ordine finale (entrambi diventano pronti dopo `mail`).

Le possibili condizioni di errore sono enumerate da `ResolutionError`:

| Variante | Quando |
|----------|--------|
| `BadVersion` / `BadRequirement` | Versione o range SemVer non parsabili. |
| `Incompatible` | Il modulo non è compatibile con la versione del framework. |
| `MissingDependency` | Una dipendenza dichiarata non è presente nel catalogo. |
| `DependencyConflict` | La dipendenza esiste ma la sua versione non soddisfa il range. |
| `DuplicateModule` | Due moduli dichiarano lo stesso `name`. |
| `SelfDependency` | Un modulo elenca se stesso tra le dipendenze. |
| `DependencyCycle` | Il grafo delle dipendenze contiene un ciclo. |

Il wrapper `resolve_modules` (in `crates/kigumi-core/src/registry.rs`) alimenta `resolve_module_set` con tutti i manifest registrati nel catalogo e con `FRAMEWORK_VERSION`.

## La chiusura delle dipendenze

Quando installi un modulo non ne installi solo quello: installi la sua **chiusura transitiva**. La funzione `module_closure(name)` (in `crates/kigumi-core/src/registry.rs`) restituisce il modulo più tutte le sue dipendenze transitive, in ordine di dipendenza (dipendenze prima):

```rust
pub fn module_closure(name: &str) -> Result<Vec<&'static str>, String> {
    let mods = resolve_modules()?; // validated + topo-sorted
    // ... raccoglie name + dipendenze transitive ...
    // Return in the validated dependency order (dependencies before dependents).
}
```

Per esempio, `module_closure("sales")` restituisce `["base", "mail", "sales"]`, mentre `module_closure("base")` restituisce `["base"]`. Un nome sconosciuto produce un errore. Poiché il risultato viene riordinato secondo l'ordine validato dei moduli, la chiusura non contiene mai un dipendente prima delle sue dipendenze.

## Install e uninstall

I moduli si gestiscono dalla CLI (`apps/kigumi-cli/src/main.rs`), sottocomando `module`:

```text
kigumi module list              # elenca i moduli linkati e se ciascuno è installato
kigumi module install <name>    # installa un modulo + la sua chiusura, poi migra le tabelle
kigumi module uninstall <name>  # disinstalla un modulo (tabelle e dati KEPT)
```

`kigumi module list` stampa una riga per modulo con nome, versione, stato (`installed` / `available`) e summary:

```text
  base       1.0.0    [installed]  Foundational models: currency, partner, company
  mail       1.0.0    [available]  Headless chatter: messages, tracking, followers, activities
  ...
```

### Install

`kigumi module install <name>` calcola la chiusura con `module_closure(&name)`, marca come installati i moduli non ancora presenti e poi richiama `migrate_installed`, che crea (idempotentemente) le tabelle dei moduli appena installati:

```rust
ModuleCmd::Install { name } => {
    let want = module_closure(&name)?; // name + transitive dependencies, deps first
    let mut any = false;
    for m in mods.iter().filter(|m| want.contains(&m.name)) {
        if !db.is_module_installed(m.name).await? {
            db.mark_module_installed(m.name, m.version).await?;
            println!("installing {} {}", m.name, m.version);
            any = true;
        }
    }
    if !any {
        println!("'{name}' and its dependencies are already installed");
    }
    migrate_installed(db).await?; // create the newly-installed modules' tables (idempotent)
}
```

### Uninstall — i dati restano

`kigumi module uninstall <name>` ha due guardie e una semantica non distruttiva:

1. **`base` non è disinstallabile** (è il modulo fondante): `cannot uninstall 'base' (the foundational module)`.
2. **Guardia a valle**: se un modulo installato dipende ancora da quello richiesto, l'uninstall è rifiutato finché non si disinstallano prima i dipendenti.
3. **I dati vengono conservati**: l'uninstall si limita a cancellare la riga dal registro di installazione (`mark_module_uninstalled`). Il modulo smette di essere migrato e servito, **ma le sue tabelle e i suoi dati restano intatti**; re-installandolo si recupera tutto.

```rust
ModuleCmd::Uninstall { name } => {
    if name == "base" {
        return Err("cannot uninstall 'base' (the foundational module)".into());
    }
    // ... guardia a valle sui dipendenti ...
    db.mark_module_uninstalled(&name).await?;
    println!("uninstalled '{name}' (its tables and data are kept; re-install to restore)");
}
```

Questo è deliberatamente non distruttivo e reversibile: l'uninstall è un *disable*, non un *drop*.

## Il registro di installazione per modulo

Lo stato "installato" vive in una tabella dedicata, `installed_module`, gestita da `crates/kigumi-db/src/module_store.rs`:

```sql
CREATE TABLE IF NOT EXISTS installed_module
  (name text PRIMARY KEY,
   installed_version text NOT NULL,
   installed_at timestamptz NOT NULL DEFAULT now())
```

Un modulo è **disponibile** quando la sua crate è linkata (compile time); è **installato** quando ha una riga qui. Le operazioni sul registro sono `installed_modules()`, `is_module_installed(name)`, `mark_module_installed(name, version)` e `mark_module_uninstalled(name)`.

Esiste una seconda tabella, `kigumi_module` (in `crates/kigumi-db/src/migration.rs`), che è il **ledger di migrazione per modello**: traccia la versione fino a cui le tabelle di ciascun modulo sono state migrate, usata da `install_or_upgrade` (con un `pg_advisory_xact_lock` per serializzare install/upgrade concorrenti dello stesso modulo). Il metodo `has_prior_migration` distingue un DB davvero nuovo da uno aggiornato prima che esistesse la selezione dei moduli.

### Migrazione guidata dall'installato

Su un database nuovo non è installato nulla, quindi `migrate` installa per primo `base` (e la sua chiusura); il resto è opt-in. Su un DB che aveva già migrazioni *prima* dell'introduzione della selezione per modulo, vengono mantenuti **tutti** i moduli già presenti, così l'upgrade non nasconde silenziosamente modelli prima disponibili:

```rust
if db.installed_modules().await?.is_empty() {
    let mods = resolve_modules()?;
    let want: Vec<&str> = if db.has_prior_migration().await? {
        mods.iter().map(|m| m.name).collect()
    } else {
        module_closure("base")?
    };
    // ... mark_module_installed per ogni modulo in `want` ...
}
```

`migrate_installed` poi migra solo i modelli dei moduli installati, **in ordine di dipendenza degli FK**, crea le tabelle di giunzione Many2many in un secondo passaggio (quando entrambi gli estremi esistono) e infine effettua il seeding dei dati di base per i moduli installati (`base` → valuta + azienda di default + sequenze; `account` → piano dei conti + giornali; `stock` → magazzino + ubicazioni di default). Allo `Serve`, il router espone **solo** i modelli dei moduli installati: un modello il cui modulo proprietario non è installato viene omesso dal catalogo servito.

## Catalogo dei moduli inclusi

Cinque moduli sono inclusi e linkati nel binario `kigumi`. Tutti dichiarano `version = "1.0.0"` e `framework = ">=0.1, <0.2"`.

| Modulo | Crate | Dipende da (verificato dal MANIFEST) |
|--------|-------|--------------------------------------|
| `base` | `kigumi-mod-base` | *(nessuna)* |
| `mail` | `kigumi-mod-mail` | `base ^1.0` |
| `sales` | `kigumi-mod-sales` | `base ^1.0`, `mail ^1.0` |
| `account` | `kigumi-mod-account` | `base ^1.0`, `mail ^1.0` |
| `stock` | `kigumi-mod-stock` | `base ^1.0`, `sales ^1.0`, `mail ^1.0` |

### `base`

La radice del grafo: nessuna dipendenza, sempre installato per primo. Spedisce i modelli fondamentali su cui costruiscono gli altri moduli.

`depends: &[]` (`modules/base/src/lib.rs`).

Modelli:

| Modello | Tabella | Note |
|---------|---------|------|
| `res.currency` | `res_currency` | Valuta dei campi monetari, condivisa fra le aziende. |
| `res.partner` | `res_partner` | Anagrafica: aziende e persone (clienti, fornitori, contatti); gerarchia `parent_id` auto-referenziale. |
| `res.company` | `res_company` | Unità di isolamento dati in multi-azienda; ha valuta (`currency_id`, richiesta) e partner (`partner_id`) collegati. |
| `res.groups` | `res_groups` | Lista (read-only) dei gruppi referenziati da ACL/rule; serve all'UI per picker e filtri. |
| `res.users` | `kigumi_user` | Proiezione read-only del sottosistema di autenticazione; tabella **esterna** (mai migrata dal metamodello), via `register_external!("res.users")`. |
| `ir.attachment` | `kigumi_attachment` | File allegato a qualunque record via link polimorfico `(res_model, res_id)`; i byte vivono nel blob store content-addressed indicizzato per `checksum`. |

Feature principali:

- **Sequenze** per la numerazione dei documenti: `base` effettua il seeding delle sequenze `SO` e `PO` (es. `SO/00001`, `PO/00001`) usate dalle azioni di conferma di sale/purchase.
- **Settings runtime** (chiave/valore tipato) gestiti via `kigumi config set/get/print`; il seeding install-time imposta `base_url` (vuoto) e `mode` (`production`) senza mai sovrascrivere una modifica dell'operatore.
- **Multi-azienda**: `res.company` è l'unità di isolamento; i modelli transazionali portano un `company_id` proprio (es. `sale.order`), mentre i partner sono condivisi.
- **Seeding di base**: su un'istanza nuova viene creata una valuta (`Euro`/`EUR`) e un'azienda (`Main Company`); `res.groups` viene popolato dai gruppi referenziati dalle ACL/rule registrate.
- ACL: il gruppo `user` legge i dati di riferimento e la lista gruppi (e può creare/modificare i partner); `res.users` e `ir.attachment` (CRUD generico) sono `admin`-only — gli utenti raggiungono i file tramite gli endpoint dedicati `/api/:name/:id/attachments` (più `/api/attachment/:aid/content` per il download e `/api/attachment/:aid` per la cancellazione), gated sull'accesso al record host.

### `mail`

Sottosistema di chatter headless. Un modello aderisce con **una riga** (`kigumi::register_mailed!("sale.order")`), senza mixin: guadagna un thread di messaggi indirizzato dal link polimorfico `(res_model, res_id)`, e il framework ripulisce quel thread alla cancellazione del record.

`depends: &[ModuleDep { name: "base", req: "^1.0" }]` (`modules/mail/src/lib.rs`). Dipende da `base` perché `res.users` è l'autore del messaggio / assegnatario dell'attività.

Modelli:

| Modello | Tabella | Note |
|---------|---------|------|
| `mail.message` | `mail_message` | Messaggio del thread (commento o nota di sistema); append-only, ordinato per `id`; `parent_id` per le risposte annidate. |
| `mail.tracking` | `mail_tracking` | Riga di audit di un cambio campo: una coppia `old_value` / `new_value` tipizzata, portata da un messaggio `notification`. |
| `mail.activity` | `mail_activity` | To-do schedulato su un record (`date_deadline` + `user_id` assegnatario); lo `state` (overdue/today/planned) è **derivato** da `date_deadline` in lettura, mai memorizzato. |
| `mail.follower` | `mail_follower` | Sottoscrizione al thread di un record; unicità di `(res_model, res_id, user_id)` via indice composito (`ensure_mail_indexes`). |

Feature principali:

- **Chatter** via gli endpoint dedicati `/api/:name/:id/messages` (GET) e `/api/:name/:id/message` (POST), gated sull'accesso in lettura al record host: un utente posta/legge i thread solo dei record che può già vedere.
- **Tracking** dei campi marcati `tracked` nei modelli (es. `state` su `sale.order`).
- **Followers** e **activities** sullo stesso link polimorfico (endpoint `/api/:name/:id/followers`, `/follow`, `/unfollow`, `/activities`, `/activity`, `/activities/:aid/done`).
- **Opt-in via `register_mailed!`**: oltre ai modelli di altri moduli, `mail` retrofitta `res.partner` (`register_mailed!("res.partner")`), così `base` non ha bisogno di dipendere da `mail` (la freccia va sempre `mail → base`).
- ACL: i modelli del thread sono `admin`-only sulle route CRUD generiche (moderazione/debug); l'accesso normale passa per gli endpoint chatter, che agiscono in modo elevato dopo il controllo sull'host.

### `sales`

Gestione delle vendite e degli acquisti, con catalogo prodotti, varianti, listini e tasse.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/sales/src/lib.rs`). `sale.order` e `product.template` aderiscono al chatter (`register_mailed!`), da cui la dipendenza da `mail`.

Modelli:

| Modello | Tabella | Note |
|---------|---------|------|
| `product.category` | `product_category` | Categoria gerarchica di prodotti. |
| `uom.uom` | `uom_uom` | Unità di misura con rapporto (`factor`) rispetto al riferimento di categoria. |
| `product.template` | `product_template` | Definizione condivisa del prodotto (campi comuni a tutte le varianti); aderisce al chatter. |
| `product.product` | `product_product` | Variante vendibile; `inherits = "product.template"` via `product_tmpl_id`; porta riferimento interno (`default_code`), barcode, tag, prezzo extra (`price_extra`) e giacenza (`qty_available`). |
| `product.tag` | `product_tag` | Etichetta/tag di variante (comodel del Many2many `tag_ids`). |
| `product.attribute` | `product_attribute` | Dimensione configurabile (es. "Color"). |
| `product.attribute.value` | `product_attribute_value` | Valore possibile di un attributo (es. "Red"). |
| `product.template.attribute.line` | `product_template_attribute_line` | Riga d'attributo su un template: quali valori sono selezionati. |
| `product.template.attribute.value` | `product_template_attribute_value` | Cella per-template di un valore scelto; gli FK strutturali sono engine-locked (`groups = "base.system"`), il solo `price_extra` è editabile da un manager. |
| `product.pricelist` | `product_pricelist` | Listino in una valuta. |
| `product.pricelist.item` | `product_pricelist_item` | Regola di listino con scope (`applied_on`: variante > prodotto > categoria > globale) e `compute_price` (fisso o sconto %). |
| `account.tax` | `account_tax` | Tassa (sottoinsieme minimale, percentuale per riga); è prevista una sua estensione da parte del modulo `account` via `#[extend]`. |
| `sale.order` | `sale_order` | Ordine di vendita; `state` e `invoice_status` tracciati; aggregati `amount_untaxed`/`amount_tax`/`amount_total` calcolati dalle righe; l'estensione `sale_margin` aggiunge `margin` via `#[extend]`. |
| `sale.order.line` | `sale_order_line` | Riga ordine: prodotto, quantità, prezzo, sconto, tassa; `price_subtotal`/`price_tax`/`price_total`/`margin` come compute store. |
| `purchase.order` | `purchase_order` | Mirror buy-side di `sale.order`. |
| `purchase.order.line` | `purchase_order_line` | Riga d'acquisto (stessa forma della riga vendita, riusa gli stessi compute). |
| `sale.order.discount` | `sale_order_discount` | Wizard transient (`register_transient!`) per applicare uno sconto % a tutte le righe di un ordine. |

Azioni e metodi di servizio:

- **Azioni di stato**: `confirm` e `done` su `sale.order` (`confirm` assegna il numero SO dalla sequenza e imposta `invoice_status = to_invoice`); `confirm` e `done` su `purchase.order` (`confirm` assegna il numero PO). Esposte come `POST /api/:name/:id/action/:action`.
- **`generate_variants`** — materializza le combinazioni di attributi di un `product.template` in varianti `product.product`. `POST /api/:name/:id/generate_variants` (valido solo su `product.template`).
- **`apply_pricelist`** — ri-prezza le righe di un `sale.order` a partire dal suo listino (stessa valuta). `POST /api/:name/:id/apply_pricelist` (valido solo su `sale.order`).
- **Wizard sconto** — `POST /api/:name/open` apre il wizard transient (seeding via `default_get`), e `POST /api/:name/:id/apply_discount` scrive lo sconto sulle righe dell'ordine target (valido solo su `sale.order.discount`).
- **`create_invoice`** — genera una fattura cliente postata (`account.move`) da un `sale.order` confermato; richiede il modulo `account` installato (altrimenti l'errore `install the account module to invoice`) e flippa `invoice_status` a `invoiced`. `POST /api/:name/:id/create_invoice` (valido solo su `sale.order`).
- **`create_delivery`** — crea un trasferimento di consegna dalle righe di un `sale.order`; richiede il modulo `stock` (altrimenti `install the stock module to create transfers`). `POST /api/:name/:id/create_delivery` (valido solo su `sale.order`).
- **`create_receipt`** — crea un trasferimento di ricezione dalle righe di un `purchase.order`; richiede il modulo `stock`. `POST /api/:name/:id/create_receipt` (valido solo su `purchase.order`).
- **Report** `quotation` su `sale.order` (HTML, con escaping del contenuto memorizzato). `GET /api/:name/:id/report/:report`.
- ACL: i gruppi `sales.user` (operatività ordini/righe) e `sales.manager` (manutenzione del catalogo); le purchase order sono gestite pragmaticamente dagli stessi gruppi. Le record rule limitano la visibilità agli ordini non "done" (e ai soli ordini piccoli, sotto 10.000, per `sales.user`).

### `account`

Partita doppia headless: un libro mastro generale.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/account/src/lib.rs`). `account.move` aderisce al chatter (audit trail), da cui la dipendenza da `mail`.

Modelli:

| Modello | Tabella | Note |
|---------|---------|------|
| `account.account` | `account_account` | Conto del piano dei conti; `account_type` guida il comportamento (receivable/payable/income/expense/tax…). |
| `account.journal` | `account_journal` | Giornale; `code`/`sequence_code` guidano la numerazione delle registrazioni postate. |
| `account.move` | `account_move` | Registrazione/fattura: raggruppa le righe dare/avere; mailed; numerata `/` finché non postata; `amount_total` è un aggregato store. |
| `account.move.line` | `account_move_line` | Riga di registrazione: una scrittura su un conto GL; dare XOR avere (due colonne `Decimal`); `balance = debit − credit` derivato in lettura. |

Feature principali:

- **Posting in partita doppia**: `POST /api/:name/:id/post` posta una registrazione in bozza (ri-controllo del bilanciamento + numerazione per giornale + `state → posted`); valido solo su `account.move`.
- **Vincolo di entry bilanciata** (`check_balanced`, `register_constraint!`): il totale dare di una registrazione deve uguagliare il totale avere — un vincolo cross-record che un CHECK SQL a singola riga non può esprimere. Una registrazione vuota (Σ = 0) è bilanciata. Un secondo vincolo (`check_line_companies`) impedisce di mischiare aziende in una stessa entry.
- **Immutabilità del postato**: record rule freezano le righe di una `account.move` postata (no write/create/delete) — è ciò che garantisce l'invariante "posted ⇒ balanced". Le azioni `button_draft` e `button_cancel` gestiscono i ritorni di stato.
- **Fatturazione vendite**: è l'altro estremo del `create_invoice` di `sales` — `Db::create_sale_invoice` genera e posta una `account.move` cliente da un ordine confermato.
- **Seeding**: quando `account` è installato, `migrate_installed` effettua il seeding di un piano dei conti minimale + giornali (Customer Invoices/Vendor Bills/Bank/Miscellaneous) per l'azienda di default.
- ACL: `account.user` (contabile) e `account.manager` (configurazione: creazione conti, manutenzione giornali, cancellazione registrazioni).

### `stock`

Libro di magazzino headless: ubicazioni, magazzini, giacenze, trasferimenti e movimenti.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "sales", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/stock/src/lib.rs`). Dipende da `base` (azienda), `sales` (`product.product`) e `mail` (i trasferimenti portano un thread di chatter).

Modelli:

| Modello | Tabella | Note |
|---------|---------|------|
| `stock.location` | `stock_location` | Ubicazione; `usage` guida il comportamento — solo `internal` conta come giacenza reale, supplier/customer/inventory/transit sono virtuali. |
| `stock.warehouse` | `stock_warehouse` | Magazzino: un'ubicazione interna con un codice breve. |
| `stock.quant` | `stock_quant` | Giacenza di un prodotto in un'ubicazione (materializzata); unica su `(product_id, location_id)` via `ensure_stock_indexes`. |
| `stock.picking` | `stock_picking` | Trasferimento (receipt/delivery/internal): documento che raggruppa i movimenti da una sorgente a una destinazione; mailed; `state` tracciato. |
| `stock.move` | `stock_move` | Movimento di un prodotto all'interno di un trasferimento; done quando il trasferimento è validato. |

Feature principali:

- **Meccanismo `validate`**: `POST /api/:name/:id/validate` (`Db::validate_picking`) valida un trasferimento in bozza — assegna il numero dalla sequenza per tipo (`receipt`→`IN`, `delivery`→`OUT`, altrimenti `INT`), porta i movimenti a `done` con un compare-and-set (`FOR UPDATE` + ri-asserzione di `draft`) per impedire doppie validazioni concorrenti, e aggiorna le giacenze (`stock.quant`). Valido solo su `stock.picking`.
- **Integrazione con gli ordini**: i metodi `create_delivery` (da `sale.order`) e `create_receipt` (da `purchase.order`) del modulo `sales` generano i `stock.picking` corrispondenti; la giacenza materializzata `product.product.qty_available` viene aggiornata dal meccanismo di move-done.
- **Immutabilità del done**: record rule freezano i movimenti di un trasferimento `done` (no write/create/delete) — l'analogo stock di una registrazione contabile postata.
- **Seeding**: quando `stock` è installato, `migrate_installed` effettua il seeding di un magazzino di default + le ubicazioni standard (Stock / Vendors / Customers / Inventory adjustment) per l'azienda di default.
- ACL: `stock.user` (operatività trasferimenti/movimenti) e `stock.manager` (configurazione: ubicazioni, magazzini, modifica diretta delle giacenze).

## Grafo delle dipendenze e ordine di installazione

Le dipendenze dichiarate nei manifest formano questo grafo aciclico. In forma di archi (`modulo → dipendenze`):

- `base` → *(nessuna)*
- `mail` → `base`
- `sales` → `base`, `mail`
- `account` → `base`, `mail`
- `stock` → `base`, `sales`, `mail`

Rappresentato come grafo (le frecce vanno dal dipendente alle sue dipendenze; `base` è la radice in basso):

```text
   stock
   ├──► sales ──► mail ──► base
   ├──► mail ──────────────► base
   └──► base

   account
   ├──► mail ──► base
   └──► base
```

`base` non dipende da nulla; `mail` dipende solo da `base`; `sales` e `account` dipendono entrambi da `base` e `mail`; `stock` dipende da `base`, `sales` e `mail`.

`resolve_module_set` ordina topologicamente questo grafo (Kahn, con tiebreak alfabetico). L'ordine di installazione risultante (dipendenze prima dei dipendenti), confermato da `kigumi version`, è:

1. `base`
2. `mail`
3. `account`
4. `sales`
5. `stock`

`account` precede `sales` perché entrambi diventano "pronti" appena installato `mail`, e a parità di disponibilità il tiebreak è alfabetico (`account` < `sales`).

Concretamente, `module_closure` produce le chiusure attese (sempre nell'ordine validato, dipendenze prima):

- `module_closure("base")` → `["base"]`
- `module_closure("sales")` → `["base", "mail", "sales"]`
- `module_closure("stock")` → `["base", "mail", "sales", "stock"]`

`base` è sempre installato per primo su un database nuovo; gli altri moduli sono opt-in via `kigumi module install <name>`.

## Vedi anche

- [moduli-custom.md](./moduli-custom.md) — come scrivere un modulo proprio (manifest, modelli, ACL, azioni).
- [architettura.md](./architettura.md) — il registro a compile time e il metamodello.
- [sicurezza.md](./sicurezza.md) — ACL, record rule e sicurezza a livello di campo.
- [api.md](./api.md) — il contratto-UI e gli endpoint REST.
- [installazione.md](./installazione.md) e [configurazione.md](./configurazione.md) — bootstrap e configurazione dell'istanza.
