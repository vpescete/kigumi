# Modello di sicurezza

Kigumi è headless e schema-driven: ogni accesso ai dati passa per un solo confine — i metodi `*_secured` del crate [`kigumi-db`](architettura.md) — dove l'autenticazione produce un'identità fidata (`Ctx`) e l'autorizzazione viene applicata in un unico punto, su ogni lettura e scrittura. Questa pagina descrive l'autenticazione (token JWT HS256, revoca, rotazione del segreto), i livelli di autorizzazione (ACL, record rule, scope multi-azienda, gruppi a livello di campo, sudo / effetti elevati), l'AST dei domini e la validazione dell'input al confine di scrittura, con linee guida pratiche per chi scrive un modulo. Per la panoramica vedi [README.md](README.md), per l'architettura [architettura.md](architettura.md), per le route REST [api.md](api.md), per la configurazione [configurazione.md](configurazione.md).

## Autenticazione

L'autenticazione vive nel crate `kigumi-auth` (`crates/kigumi-auth/src/lib.rs`). I token sono **JWT firmati HS256** con un segreto condiviso; la crittografia è delegata al crate `jsonwebtoken`, le password usano `argon2`.

### Token tipizzati: access e refresh

Esistono due tipi di token, distinti dalla claim `kind`:

| Token | `kind` | TTL effettivo | Contenuto (claim) | A cosa serve |
|-------|--------|---------------|-------------------|--------------|
| **access** | `"access"` | `ACCESS_TTL` = `900` s (15 min) | `sub` (uid), `kind`, `groups`, `company`, `companies`, `exp` | Bearer per ogni richiesta dati: verifica in un `Ctx` fidato |
| **refresh** | `"refresh"` | `REFRESH_TTL` = `2_592_000` s (30 giorni) | `sub` (uid), `kind`, `jti`, `exp` | Prova l'identità per emettere un nuovo access token; mai usato come bearer |

I TTL effettivi sono costanti del server (`crates/kigumi-server/src/lib.rs`):

```rust
const ACCESS_TTL: u64 = 900; // 15 minutes
const REFRESH_TTL: u64 = 2_592_000; // 30 days
```

Il file `kigumi.toml` espone una sezione `[auth]` con gli stessi valori di default (`access_ttl = 900`, `refresh_ttl = 2592000`), e `kigumi-config` ne definisce i default uguali; in v1 l'emissione dei token usa direttamente le costanti del server (`issue_token_pair`), quindi questi sono i valori effettivi a runtime.

La separazione tra i due tipi è una garanzia esplicita: la claim `kind` viene verificata su ogni decodifica (`decode_kind`), quindi **un refresh token non può mai essere usato come bearer per accedere ai dati** e viceversa. Questo impedisce a un refresh token a lunga vita di agire da bearer onnipotente.

```rust
fn decode_kind(&self, token: &str, kind: &str) -> Result<Claims, AuthError> {
    // Pin HS256 (rejects alg=none/confusion) and validate exp with no grace window.
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(self.secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AuthError::Invalid)?;
    if data.claims.kind != kind {
        return Err(AuthError::Invalid);
    }
    Ok(data.claims)
}
```

L'algoritmo è fissato a `HS256` in fase di verifica: questo rifiuta i token con `alg=none` e gli attacchi di confusione di algoritmo. La scadenza è validata senza finestra di tolleranza (`leeway = 0`).

### Il `Ctx` fidato derivato dal Bearer

Una richiesta dati presenta un header `Authorization: Bearer <token>`. Il server lo verifica in un `Ctx` — l'identità fidata che attraversa tutto il motore di sicurezza (`authenticate` in `crates/kigumi-server/src/lib.rs`):

```rust
/// Verifies the request's bearer token into a trusted `Ctx`, or a 401 response. This is real
/// authentication: a client cannot claim a group without a token signed by the server secret.
fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    backend
        .auth
        .verify_bearer(header)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}
```

`verify_bearer` estrae il prefisso `Bearer `, verifica il token come **access** (`verify_access`) e costruisce il `Ctx`. Poiché i `groups` e lo scope aziendale viaggiano firmati dentro il token, **un client non può rivendicare un gruppo senza un token firmato dal segreto del server**: non c'è round-trip aggiuntivo al database per ogni richiesta.

Il `Ctx` (definito in `crates/kigumi-core/src/security.rs`) trasporta:

```rust
pub struct Ctx {
    pub uid: i64,
    pub groups: Vec<String>,
    su: bool,                              // privato: nessuno può forgiare un contesto elevato
    pub company_id: Option<i64>,           // azienda attiva
    pub allowed_company_ids: Vec<i64>,     // aziende accessibili (lo scope multi-azienda)
}
```

Il flag `su` è **privato**: codice esterno non può costruire un `Ctx { su: true, .. }` con un literal di struct. L'unico modo per elevare un contesto è il metodo greppabile `Ctx::sudo()`.

L'endpoint `GET /auth/me` restituisce esattamente i campi del `Ctx` ricavato dal token (`uid`, `groups`, `company_id`, `allowed_company_ids`).

### Ciclo di vita del token: login, refresh, logout

| Route | Corpo | Effetto |
|-------|-------|---------|
| `POST /auth/login` | `{ "login", "password" }` | Verifica le credenziali (argon2) ed emette la coppia access+refresh |
| `POST /auth/refresh` | `{ "refresh_token" }` | Reclama (revoca) il refresh presentato ed emette una coppia nuova (rotazione) |
| `POST /auth/logout` | `{ "refresh_token" }` | Revoca il refresh token presentato (risponde sempre `204`) |
| `GET /auth/me` | — | Restituisce l'identità (`Ctx`) del bearer presentato |

Il login esegue **sempre** argon2 — contro un hash fittizio (`dummy_hash`) se l'utente è sconosciuto — così che tempi di risposta e corpo del 401 siano identici per utente inesistente e password errata (nessuna user enumeration via timing). Su refresh, `groups` e scope aziendale vengono **riletti dal database** (`user_groups` e `user_scope`), così che riassegnazioni di gruppo o azienda abbiano effetto senza re-login.

### Revoca del token (jti)

I refresh token sono **stateful**: ognuno è registrato per `jti` nella tabella `kigumi_refresh` (`crates/kigumi-db/src/auth_store.rs`), quindi può essere revocato (logout) e ruotato (ogni refresh invalida il precedente). Un refresh token rubato ma revocato viene rifiutato.

La rotazione su refresh è atomica e a prova di replay: `claim_refresh` controlla e revoca in **un solo statement** SQL, così due reclami concorrenti dello stesso token non possono entrambi avere successo (chi perde aggiorna zero righe → rifiutato), prevenendo il double-spend.

```rust
/// Atomically claims (revokes) an active refresh token, returning its user id. The check and
/// the revoke happen in ONE statement, so two concurrent claims of the same token cannot both
/// succeed: the loser's UPDATE affects zero rows → `None`. This prevents refresh double-spend.
pub async fn claim_refresh(&self, jti: &str) -> Result<Option<i64>, DbError> {
    let row = sqlx::query(
        "UPDATE kigumi_refresh SET revoked = true \
         WHERE jti = $1 AND NOT revoked AND expires_at > now() RETURNING user_id",
    )
    .bind(jti)
    .fetch_optional(&self.pool)
    .await?;
    Ok(row.map(|r| r.get("user_id")))
}
```

Gli access token, al contrario, sono **stateless e a breve vita**: non vengono tracciati. La revoca immediata vale per il refresh; un access token resta valido fino alla sua scadenza (15 minuti). È esattamente questa coppia access-breve / refresh-lungo-revocabile a rendere utile la separazione.

### Rotazione del segreto: `KIGUMI_JWT_SECRET` e `KIGUMI_JWT_SECRET_OLD`

I segreti sono letti **solo dall'ambiente**, mai da `kigumi.toml`, e la presenza di quelli obbligatori è verificata al boot (fail-fast). Estratto da `.env.example`:

```bash
# REQUIRED — HS256 signing secret for access/refresh tokens.
KIGUMI_JWT_SECRET=CHANGE_ME_long_random_value
# OPTIONAL — previous JWT secret, still accepted on verify during a rotation window.
# KIGUMI_JWT_SECRET_OLD=
```

`Secrets::from_env` (`crates/kigumi-config/src/secrets.rs`) carica `KIGUMI_JWT_SECRET` come obbligatorio e `KIGUMI_JWT_SECRET_OLD` come opzionale:

```rust
jwt_secret: req("KIGUMI_JWT_SECRET")?,
jwt_secret_old: opt("KIGUMI_JWT_SECRET_OLD"),
```

Il modello di rotazione previsto è: si imposta `KIGUMI_JWT_SECRET_OLD` al segreto precedente quando si introduce un nuovo `KIGUMI_JWT_SECRET`; durante la finestra di rotazione i token firmati con il segreto vecchio restano accettati in verifica, mentre i nuovi token vengono firmati con quello nuovo. Entrambi i segreti compaiono (mascherati) nel riepilogo di configurazione del server.

> **Nota di implementazione (v1)**: `KIGUMI_JWT_SECRET_OLD` è già letto e propagato in `Secrets.jwt_secret_old` (e mostrato mascherato nel riepilogo di configurazione), ma `Authenticator::new(...)` accetta **un solo segreto** (`pub struct Authenticator { secret: String }`) e il comando `kigumi serve` cabla solo `s.secrets.jwt_secret`. La verifica con il segreto vecchio non è quindi ancora attiva nel percorso runtime: il cablaggio del secondo segreto in `Authenticator` è il passo che completa la rotazione senza invalidare i token in volo. Vedi [Incertezze](#incertezze-e-note) e l'`Authenticator` in `crates/kigumi-auth/src/lib.rs`.

## Autorizzazione: un solo punto di applicazione

L'autorizzazione non è sparsa nei controller: vive nei metodi `*_secured` di `kigumi-db` (`crates/kigumi-db/src/lib.rs`), attraversati da **ogni** lettura e scrittura protetta. I controlli che entrano in gioco sono, in tutti i casi:

- **ACL** del modello per l'operazione (`check_access`) — default-deny;
- **gruppi a livello di campo** sui campi toccati (`field_accessible`, via `strip_unreadable` / `check_writable_fields` / vincoli su filter e order-by);
- **record rule** del modello per l'operazione, compilate nel `WHERE` (`record_rule_domain`);
- **scope multi-azienda** (`apply_company_scope` in scrittura, `company_filter` / `company_clause` in lettura) — default-deny sulle righe condivise;
- in scrittura, **validazione dell'input** (`validate_write_values`: required, tipi, rifiuto dei campi computed).

L'ordine esatto dipende dal verso dell'operazione:

- **In lettura** (`read_secured` / la costruzione del dominio di ricerca): prima l'ACL `Read`, poi il dominio di record rule e quello multi-azienda vengono **AND-ati** dentro il `WHERE`; un filtro o un order-by del client che referenzia un campo non leggibile è rifiutato; dopo il fetch, `strip_unreadable` rimuove dalle righe i campi non leggibili.
- **In scrittura** (`insert_secured` / `update_secured` / `delete_secured`): prima l'ACL (`Create`/`Write`/`Delete`), poi `check_writable_fields` (gruppi di campo), poi `apply_company_scope`, poi `validate_write_values`; la record rule dell'operazione è infine compilata nel `WHERE` dell'`INSERT … WHERE`/`UPDATE … WHERE`/`DELETE … WHERE` eseguito, così la riga è toccata solo se la regola la ammette.

Il superuser (`Ctx::sudo()`) bypassa ACL, record rule e scope aziendale; resta soggetto solo alla coerenza del dato (constraint, validazione dei tipi).

### ACL: concessione modello + gruppo

Un'`Acl` concede a un **gruppo** i quattro permessi su un **modello** (`crates/kigumi-core/src/security.rs`):

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

La verifica è a **default-deny** con semantica di **unione**: l'accesso è concesso se *almeno uno* dei gruppi dell'utente concede l'operazione; il superuser è sempre ammesso.

```rust
pub fn check_access(op: Operation, model: &str, ctx: &Ctx, acls: &[Acl]) -> bool {
    if ctx.su {
        return true;
    }
    acls.iter()
        .any(|a| a.model == model && ctx.is_member(a.group) && a.grants(op))
}
```

Le `Operation` sono `Read`, `Write`, `Create`, `Delete`. Un modulo dichiara le sue ACL come slice statica e le registra; il server raccoglie l'unione di tutte le ACL registrate nei moduli linkati via `registered_acls()` (`crates/kigumi-core/src/registry.rs`). I gruppi distinti referenziati da ACL e record rule sono ricavabili con `registered_group_names()` (la fonte per il seeding della lista read-only `res.groups`).

Esempio reale (modulo `account`): `account.user` gestisce i movimenti (`account.move`) ma non li cancella; la configurazione (creare conti, mantenere i giornali, cancellare i movimenti) è riservata a `account.manager`:

```rust
pub static ACLS: &[Acl] = &[
    Acl { model: "account.account", group: "account.user", read: true, write: true, create: false, delete: false },
    Acl { model: "account.account", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.journal", group: "account.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.journal", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.move", group: "account.user", read: true, write: true, create: true, delete: false },
    Acl { model: "account.move", group: "account.manager", read: true, write: true, create: true, delete: true },
    // ...
];
kigumi::register_acls!(ACLS);
```

### Record rule: filtri di dominio per riga

Una `RecordRule` restringe a livello di **riga**: applica un `Domain` tipato alle operazioni indicate, per i gruppi indicati (`crates/kigumi-core/src/security.rs`):

```rust
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],   // vuoto = globale (vale per tutti)
    pub ops: &'static [Operation],
    pub domain: RuleDomain,
}
```

Il dominio di una regola può essere **statico** o **già materializzato**, distinti da `RuleDomain`:

```rust
pub enum RuleDomain {
    Static(fn() -> Domain),   // regola di modulo a compile time (thunk: un Domain non è const)
    Owned(Domain),            // regola caricata a runtime (es. dal database), dominio già materializzato
}
```

Il motore tratta i due casi in modo identico — cambia solo da dove proviene il dominio — quindi regole statiche e regole DB confluiscono in un'unica lista senza casi speciali (`RuleDomain::resolve` chiama il thunk o clona il valore).

La combinazione delle regole applicabili a `(op, model, ctx)` segue una semantica precisa (`record_rule_domain`): le regole **globali** (senza gruppo) sono *tutte* richieste → in **AND**; le regole dei gruppi a cui l'utente appartiene sono alternative → in **OR**; i due blocchi vengono poi messi in **AND**. Il superuser non è ristretto da alcuna regola (`record_rule_domain` restituisce `None`).

Esempio reale: il **freeze dei movimenti contabili registrati** (modulo `account`). Le righe di un movimento `posted` sono congelate — niente write, create o delete — così da garantire l'invariante "posted ⇒ balanced". È una regola **globale** (`groups: &[]`) che attraversa la relazione `move_id.state`:

```rust
fn line_move_not_posted() -> Domain {
    Domain::field("move_id.state").ne("posted")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Write],  domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(line_move_not_posted) },
];
kigumi::register_rules!(RECORD_RULES);
```

L'analogo per lo stock (modulo `stock`) è il **freeze del trasferimento validato**: le righe di un trasferimento `done` sono congelate (solo sudo o l'annullamento possono toccarle), via `picking_id.state`:

```rust
fn move_picking_not_done() -> Domain {
    Domain::field("picking_id.state").ne("done")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Write],  domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(move_picking_not_done) },
];
kigumi::register_rules!(RECORD_RULES);
```

La regola raggiunge `move_id.state` / `picking_id.state` tramite un dominio dotato: questo copre sia il path diretto della riga sia il path annidato `line_ids` (le scritture sui figli ri-verificano le record rule del figlio).

### Scoping multi-azienda

Un modello è **scoped per azienda** quando dichiara un `Many2one` chiamato `company_id`. Lo scope deriva dal `Ctx`: `company_id` è l'azienda attiva, `allowed_company_ids` l'insieme accessibile.

In **lettura**, `company_filter` produce il vincolo (`crates/kigumi-db/src/lib.rs`), con **default-deny sulle righe condivise**:

```rust
fn company_filter(model: &ResolvedModel, ctx: &Ctx) -> Option<Domain> {
    if !ctx.company_scoped() {
        return None;                    // solo sudo è non vincolato
    }
    // ... il modello deve avere un Many2one company_id ...
    let shared = Domain::field("company_id").is_null();
    Some(if ctx.allowed_company_ids.is_empty() {
        shared // default-deny: nessuna assegnazione → solo righe condivise (company NULL)
    } else {
        Domain::field("company_id").in_(ctx.allowed_company_ids.clone()).or(shared)
    })
}
```

Un `company_id` NULL è una riga **condivisa**, visibile a ogni azienda. Qualsiasi chiamante non-superuser è **sempre** vincolato: con un insieme accessibile vede quelle aziende più le righe condivise; con un insieme **vuoto** vede solo le righe condivise (mai "tutto"). Solo `sudo` è non ristretto.

In **scrittura**, `apply_company_scope` è il punto di applicazione unico (riusato da create del padre, create del figlio annidato e update):

- un `company_id` esplicito deve essere un id **dentro** l'insieme accessibile (non si scrive una riga in un'azienda estranea);
- un `company_id` esplicito **NULL** è privilegiato (pubblicare una riga come condivisa): un chiamante ristretto non può;
- in **create** un `company_id` non impostato è defaultato all'azienda attiva del chiamante (o all'unica azienda accessibile); un chiamante ristretto senza azienda attiva non può creare una riga scoped.

#### Come sudo bypassa lo scope

Lo scope multi-azienda è governato da `Ctx::company_scoped()`, che è vero per *qualunque* chiamante non-superuser:

```rust
pub fn company_scoped(&self) -> bool {
    !self.su
}
```

Quindi `company_filter` restituisce `None` (nessun vincolo di lettura) e `apply_company_scope` salta i controlli restrittivi **solo** per un `Ctx` elevato. È esattamente questo che permette agli effetti di sistema (vedi sotto) di leggere/scrivere righe di qualunque azienda quando girano elevati.

### Gruppi a livello di campo (`groups=`)

L'attributo `groups=` su un campo **nasconde il campo** agli utenti che non appartengono ad almeno uno dei gruppi indicati. È una restrizione fuori banda: non aggiunge colonne al metamodello, è emessa da `#[field(groups = "...")]` come `FieldGroupRegistration` (`crates/kigumi-core/src/security.rs`). Read **e** write sono gateati dallo stesso insieme, al confine del database.

```rust
pub fn field_accessible(model: &str, field: &str, ctx: &Ctx) -> bool {
    if ctx.is_su() {
        return true;
    }
    match field_required_groups(model, field) {
        None => true,                                        // default-allow se senza restrizione
        Some(groups) => groups.iter().any(|g| ctx.is_member(g)),
    }
}
```

L'applicazione è completa e simmetrica:

- in **lettura** i campi non leggibili vengono rimossi dalla riga (`strip_unreadable`); inoltre un chiamante non può **ordinare** per un campo che non può leggere (perdita d'informazione via ordinamento, `operation: "order by (restricted field)"`) né **filtrare** su di esso — un filtro fornito dal client che referenzia un campo ristretto (anche attraverso una relazione, es. `partner_id.secret`) viene rifiutato con `AccessDenied` (`filter_path_accessible`);
- in **scrittura** `check_writable_fields` rifiuta ogni campo del payload non scrivibile dal chiamante:

```rust
fn check_writable_fields(
    model: &ResolvedModel,
    ctx: &Ctx,
    payload: &Map<String, Json>,
) -> Result<(), DbError> {
    if ctx.is_su() {
        return Ok(());
    }
    for k in payload.keys() {
        if !field_accessible(model.name, k, ctx) {
            return Err(DbError::AccessDenied {
                model: model.name.to_string(),
                operation: "write (restricted field)",
            });
        }
    }
    Ok(())
}
```

La restrizione è consapevole della delega `_inherits` (una restrizione su un campo delegato vive sul modello padre) e degli shadow (un campo che il figlio dichiara come propria colonna non eredita la restrizione del padre).

Esempio reale (modulo `sales`): i campi strutturali "engine-LOCKED" sono protetti con `groups = "base.system"`, un gruppo che nessun utente possiede, così solo il motore di generazione (che gira `sudo`) può scriverli:

```rust
#[field(label = "Variant Extra Price", default = "0", groups = "base.system")]
price_extra: Decimal,

#[field(label = "On Hand", default = "0", groups = "base.system")]
qty_available: Decimal,

#[field(label = "Attribute Values", target = "product.template.attribute.value",
        relation = "variant_ptav_rel", column = "product_id", target_column = "ptav_id",
        groups = "base.system")]
product_template_attribute_value_ids: Many2many,
```

Si può anche dichiarare a mano con `kigumi::register_field_groups!("res.users", "login", &["admin"]);`.

### sudo / effetti elevati

`sudo` è un'escalation **esplicita e tipata**, non un metodo che bypassa silenziosamente i controlli:

```rust
/// Returns an elevated copy that bypasses access control. Explicit and greppable.
pub fn sudo(&self) -> Ctx {
    Ctx { su: true, ..self.clone() }
}
```

Il pattern operativo è: **un effetto di sistema autorizzato da un gate di livello superiore**. Il chiamante deve essere autorizzato sull'operazione di alto livello; superato quel gate, gli effetti collaterali del motore girano elevati, così l'utente non deve detenere anche i permessi di basso livello.

Due esempi reali in `crates/kigumi-db/src/lib.rs`:

- **Fatturazione** (`create_sale_invoice`): genera una fattura cliente (`account.move`) registrata da un ordine confermato. È gateata sul `Write` dell'ordine da parte del chiamante (`check_access(Operation::Write, "sale.order", ...)`), e l'ordine viene **reclamato** prima (il flip `invoice_status` da `to_invoice` a `invoiced` sotto il chiamante, che applica ACL **e** record rule + azienda dell'ordine, richiedendo esattamente una riga). Solo dopo, la registrazione contabile (GL) gira elevata (`ctx.sudo()`), così un commerciale non deve possedere anche i gruppi `account`. L'effetto elevato non parte mai se il chiamante non è davvero autorizzato a scrivere l'ordine.
- **Validazione di un trasferimento** (`validate_picking`): porta un `stock.picking` da `draft` a `done` in una transazione, muovendo le quantità tra i quant. È gateata sul `Write` del trasferimento; le mutazioni dei quant sono un effetto di sistema eseguito nella transazione (con `FOR UPDATE` sul picking per un vero compare-and-set).

Analogamente, le azioni di transizione di stato hanno un gate di gruppo proprio oltre al `Write`: in `run_action_secured`, se l'azione dichiara dei gruppi, un chiamante non-superuser deve appartenervi (`operation: "action (group)"` → `AccessDenied`).

### Accesso pubblico: il primitivo del portale

Una richiesta **non autenticata** — una route di modulo registrata con `auth: false` — gira sotto l'identità **guest** riservata (`uid = -1`), che porta un solo gruppo: `PUBLIC_GROUP` (`"public"`, da `kigumi-core`). È il primitivo su cui si costruisce un portale pubblico (tracciamento ordine, download fattura, catalogo pubblicato), usando i layer sopra — nessun meccanismo nuovo:

1. Concedi `public` **`Read`** sul modello (un normale ACL). Finché non lo fai, il guest è default-deny su tutto: il primitivo è **inerte di default**.
2. Aggiungi una **record rule** per il gruppo `public` che lo restringe alle righe destinate a essere pubbliche (es. `website_published = true`).
3. Leggi col `Ctx` guest da una route `auth: false` (`input.ctx`) tramite i metodi `*_secured` — ACL e regola filtrano il risultato. **Non** fare `sudo()` su un endpoint pubblico; il punto è che a decidere cosa vede un guest sia il layer di sicurezza, non l'handler.

> **Footgun:** un ACL `public` **senza** una record rule corrispondente lascia il guest illimitato. Per un modello **con** `company_id`, lo scope company limita comunque il guest alle righe condivise (company `NULL`); ma un modello **senza** — un catalogo, una config o un modello di contenuti pubblicati, esattamente ciò che un portale tende a esporre — non ha quel bound, quindi al guest viene servita l'**intera tabella**. Il guest può solo `Read` (mai scrivere), ma è la record rule — non lo scope company — a curare davvero il sottoinsieme pubblico, ed è obbligatoria che il modello sia company-scoped o meno. Accompagna sempre la concessione con una regola.

Il guest non ottiene mai accesso in scrittura da `public` (concedi solo `Read`), e un ricevitore webhook HMAC sullo stesso path `auth: false` non è toccato: colpisce il default-deny prima di fare `.sudo()` esplicito dopo aver verificato la firma.

## L'AST dei domini e i suoi operatori

Un `Domain` è un AST di filtro **tipato**, validato contro il modello e compilato in **SQL parametrizzato** (`crates/kigumi-core/src/domain.rs`). I valori non sono mai interpolati nel testo SQL — vengono legati come parametri (`$1, $2, …`) — il che chiude la superficie di SQL injection e fa fallire i filtri malformati alla validazione, non in produzione.

```rust
pub enum Domain {
    True,
    False,
    Cond(Condition),                  // foglia: field <op> value
    And(Box<Domain>, Box<Domain>),
    Or(Box<Domain>, Box<Domain>),
    Not(Box<Domain>),
}
```

Gli operatori (`enum Operator`) e i loro costruttori fluenti:

| Operatore | Builder | SQL | Note |
|-----------|---------|-----|------|
| `Eq` | `.eq(v)` | `=` | `eq(Null)` diventa `IS NULL` |
| `Ne` | `.ne(v)` | `<>` | `ne(Null)` diventa `IS NOT NULL` |
| `Lt` / `Le` / `Gt` / `Ge` | `.lt` / `.le` / `.gt` / `.ge` | `<` `<=` `>` `>=` | non applicabili ai campi `Bool` |
| `In` / `NotIn` | `.in_(vs)` / `.not_in(vs)` | `IN` / `NOT IN` | lista vuota → `FALSE` / `TRUE`; `NULL` nella lista è rifiutato |
| `Like` / `ILike` | `.like(v)` / `.ilike(v)` | `LIKE` / `ILIKE` | solo su campi `Text` |
| `IsNull` / `IsNotNull` | `.is_null()` / `.is_not_null()` | `IS NULL` / `IS NOT NULL` | senza parametro legato |

Si combinano con `.and(...)`, `.or(...)`, `.not(...)`. Un esempio: `Domain::field("state").ne("done").and(Domain::field("amount_total").lt(10000_i64))` compila in `(state <> $1 AND amount_total < $2)` con i valori legati come parametri.

Punti chiave della compilazione:

- **identificatori dal modello, mai dall'input**: la colonna usata in SQL è il `field.name` del modello, non la stringa del path in ingresso;
- **path dotati attraverso relazioni**: un segmento `Many2one` diventa un subquery `fk IN (SELECT id FROM target WHERE …)`, un `One2many` `id IN (SELECT inverse FROM target WHERE …)`. Funziona uniformemente in SELECT/UPDATE/DELETE, così le record rule possono attraversare relazioni;
- **NULL gestiti correttamente**: confronti scalari con `NULL` sono normalizzati (`= NULL → IS NULL`, `!= NULL → IS NOT NULL`) o rifiutati, e i subquery sono resi NULL-safe perché un `Not(...)` attorno a una traversata si comporti correttamente;
- **validazione**: campo sconosciuto (`UnknownField`), campo non-colonna come un `One2many` (`NotAColumn`), tipo incompatibile (`TypeMismatch`, inclusi NaN/Infinity su `Decimal`/`Float`), operatore inadatto al tipo (`BadOperatorValue`), path non traversabile (`UnsupportedPath`) e relazione su un modello non registrato (`UnknownRelation`) sono tutti errori a compile/load time.

Il dominio ha un AST JSON portabile (`to_json` / `from_json`): lo stesso AST che il server compila in SQL, mai una stringa valutata. È usato per l'escape `?domain=<json>` e per le record rule autorate come dato; il risultato di `from_json` resta **non fidato** e va validato/compilato contro un modello prima dell'uso.

## Validazione dell'input al confine di scrittura

Ogni create/update protetto valida il payload in `validate_write_values` (`crates/kigumi-db/src/lib.rs`):

```rust
if !field.has_column() {
    return Err(DbError::BadInput(format!("field '{key}' is not a stored column")));
}
if field.is_computed() {
    return Err(DbError::BadInput(format!("field '{key}' is computed and not writable")));
}
if jv.is_null() && field.required {
    return Err(DbError::BadInput(format!("field '{key}' is required and cannot be null")));
}
out.push((field.name, json_to_value(field, jv)?)); // type checking
```

Le garanzie al confine di scrittura sono quindi:

- **required**: in create tutti i campi `required` (con colonna, non computed) devono essere presenti; un valore esplicito `null` su un campo required è rifiutato;
- **type checking**: ogni valore è convertito secondo il tipo del campo (`json_to_value`); un tipo incompatibile è `BadInput`;
- **i campi computed non sono scrivibili**: un campo computato è ricalcolato dal motore, e tentare di scriverlo è rifiutato esplicitamente (`'<campo>' is computed and not writable`);
- **solo colonne stored**: scrivere un campo che non è una colonna stored (relazioni gestite a parte, related) è rifiutato (`'<campo>' is not a stored column`).

Questi errori sono mappati su HTTP `400 Bad Request`; un `AccessDenied` diventa `403`, un conflitto `409`, e gli errori interni un `500` opaco che non espone schema o SQL (`write_error` / `internal_error` in `crates/kigumi-server/src/lib.rs`).

## Linee guida per chi scrive un modulo

Dichiarare la sicurezza di un modulo è dato statico, raccolto a compile time tramite il registro a compile time.

1. **ACL** — dichiara uno slice `&'static [Acl]` e registralo. Concedi il minimo per gruppo; ricorda che l'ACL è additiva (unione): aggiungere un'ACL può solo **ampliare** l'accesso, mai revocare una concessione esistente. Default-deny: ciò che non è concesso è negato.

   ```rust
   pub static ACLS: &[Acl] = &[
       Acl { model: "sale.order", group: "sales.user", read: true, write: true, create: true, delete: false },
       Acl { model: "sale.order.line", group: "sales.user", read: true, write: true, create: true, delete: true },
   ];
   kigumi::register_acls!(ACLS);
   ```

2. **Record rule** — per restringere a livello di riga, dichiara `&'static [RecordRule]` con `RuleDomain::Static(thunk)` e registralo. Usa `groups: &[]` per una regola **globale** (vale per tutti, in AND), o elenca i gruppi per una regola alternativa (in OR). Indica con `ops` le operazioni a cui si applica. Sfrutta i path dotati (`move_id.state`) per coprire sia il path diretto sia quello annidato.

   ```rust
   fn line_move_not_posted() -> Domain { Domain::field("move_id.state").ne("posted") }
   pub static RECORD_RULES: &[RecordRule] = &[
       RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(line_move_not_posted) },
   ];
   kigumi::register_rules!(RECORD_RULES);
   ```

3. **Gruppi a livello di campo** — per nascondere un campo a chi non è del gruppo, aggiungi `groups = "…"` all'attributo `#[field(...)]`. Per bloccare un campo a ogni utente (scrivibile solo dal motore via `sudo`) usa un gruppo che nessuno possiede, come `base.system`.

4. **Multi-azienda** — per rendere un modello scoped per azienda, dichiara un `Many2one company_id`. Lo scoping (lettura, scrittura, default in create, default-deny sulle righe condivise) si applica automaticamente, senza codice aggiuntivo.

5. **Effetti elevati** — quando un'operazione deve produrre un effetto di sistema (registrare in contabilità, validare un trasferimento), gatela prima sul permesso di alto livello del chiamante e poi esegui l'effetto su un `ctx.sudo()`. Mantieni il gate **prima** dell'effetto, così l'escalation non parte mai senza autorizzazione.

I gruppi referenziati dalle ACL e dalle record rule registrate sono raccolti da `registered_group_names()` e seminati nella lista read-only `res.groups` per i picker dell'interfaccia.

## Incertezze e note

- **Rotazione del segreto JWT (verify con il vecchio segreto)**: `KIGUMI_JWT_SECRET_OLD` è letto da `Secrets::from_env` e propagato in `Secrets.jwt_secret_old` (e mostrato mascherato nel riepilogo di configurazione), ma `Authenticator::new` accetta un solo segreto e il comando `kigumi serve` cabla solo `s.secrets.jwt_secret`. Nel codice attuale la verifica con il segreto precedente non è ancora attiva nel percorso runtime; il commento del codice e i file di esempio descrivono il comportamento previsto ("still accepted on verify during a rotation window"). Verificare la versione del runtime prima di affidarsi alla rotazione senza invalidare i token in volo.
- **TTL dei token vs `kigumi.toml`**: i valori effettivi sono le costanti `ACCESS_TTL` / `REFRESH_TTL` di `kigumi-server`; la sezione `[auth]` di `kigumi.toml` (`access_ttl` / `refresh_ttl`) porta gli stessi default ma in v1 non è cablata nell'emissione dei token. Verificare la versione prima di affidarsi al file di configurazione per cambiare i TTL.
- **`jti` sugli access token**: la revoca per `jti` riguarda solo i refresh token (tabella `kigumi_refresh`); gli access token sono stateless e non revocabili prima della scadenza (15 min). La claim `jti` è popolata solo sui refresh token.
