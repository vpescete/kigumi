# Operazioni — Configurazione, Backup/Restore, Neutralize

> Design doc (decisioni fissate, prima del codice). Definisce i *contract* e le scelte per tre temi
> operativi che Odoo risolve ma con limiti. Indurito con una design review avversariale (29 findings,
> 5 blocker) — le decisioni qui riflettono i fix. Tutto si appoggia su pezzi che Meshble ha già:
> versioning SemVer di framework e moduli, migrazioni versionate atomiche, metamodel ispezionabile,
> domini tipizzati, auth JWT, persistenza sqlx/Postgres.

**Principi trasversali**
- *Headless / agnostico*: niente dipendenze da un frontend o da un deployment specifico.
- *Schema-driven, fail-fast*: config e artefatti sono tipizzati e validati al confine; un errore è
  esplicito, non silenzioso (l'opposto dell'`odoo.conf`).
- *Segreti fuori dai file* e *fail-closed*: il default sicuro è "non comunicare / non distruggere".
- *Sicurezza imposta dal percorso di codice*, non solo dai dati (vedi neutralize).

Ordine implementativo: **(2) Config → API live → (1) Backup + (3) Neutralize**.

---

## 1. Backup & Restore

### Odoo e i suoi limiti
Zip monolitico `dump.sql` + `filestore/` + `manifest.json` minimale, master password. Limiti:
artefatto enorme e **accoppiato**, nessun incrementale, restore tutto-o-niente, **nessuna verifica di
compatibilità** (un dump di versione diversa rompe in silenzio).

### Design Meshble
Separare *dove vivono i binari* da *cos'è il backup*.

#### 1.1 Blob store dietro un trait (streaming, hash verificato)
I binari (allegati) vivono dietro un'astrazione intercambiabile, content-addressed.

```rust
// crates/meshble-storage
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Streams `src` in, computing sha256 as it goes; MUST reject if it != `sha256` (the
    /// content-address invariant is enforced, never trusted). Idempotent on a verified dedup hit.
    async fn put(&self, sha256: &str, src: impl AsyncRead + Send) -> Result<(), BlobError>;
    /// Streamed out (large blobs never fully buffered → avoids the restore OOM).
    async fn get(&self, sha256: &str) -> Result<impl AsyncRead + Send, BlobError>;
    async fn exists(&self, sha256: &str) -> Result<bool, BlobError>;
    async fn delete(&self, sha256: &str) -> Result<(), BlobError>; // solo GC, vedi §1.6
}
```

- `FsBlobStore` — **default**. File indirizzati per `sha256` (`<root>/ab/cd/abcd…`), dedup, immutabili.
- `S3BlobStore` — S3 / MinIO (cloud), stessa semantica.
- `DbBlobStore` — **fuori dalla v1**: i `bytea` finirebbero in `pg_dump`, contraddicendo la
  separazione (un `--db-only` conterrebbe tutti i binari). Eventualmente in seguito con semantiche di
  dump dedicate.

Modello allegato (referenzia per hash, non per percorso):
```
meshble_attachment(id, name, mime, size_bytes, sha256, store, created_at, res_model, res_id)
```
Blob immutabili e deduplicati ⇒ nessuno stato "a metà".

#### 1.2 Artefatto di snapshot (manifest-driven)
`meshble db dump` produce un tar:
```
manifest.json     # metadati + verifica compatibilità + firma
db.dump           # pg_dump -Fc (custom format)
blobs/            # (solo --full) blob content-addressed referenziati
```
```jsonc
{
  "format": 1,
  "meshble":  { "framework_version": "0.1.0" },
  "engine":   { "pg_server_version": "16.3", "pg_dump_version": "16.3", "archive_format": "custom" },
  "instance": { "name": "acme-prod", "db": "meshble_prod", "base_url": "https://erp.acme.com",
                "created_at": "2026-06-16T12:00:00Z" },
  "modules":  [ { "name": "base", "version": "1.0.0" }, { "name": "sales", "version": "0.3.1" } ],
  "migration_head": "0007_add_sale_discount",
  "blob_store": { "type": "fs", "count": 1234, "bytes": 56789012,
                  "digest": "sha256-of-sorted-member-hashes" },
  "contents": "full",                          // "db-only" | "full"
  "encryption": { "alg": "age", "key_id": "ops-2026" },   // assente se non cifrato
  "checksums": { "db_dump_sha256": "…" },
  "signature": "…"                              // firma del manifest (autenticità, vedi §1.7)
}
```
Più ricco di Odoo: riusa versioni moduli + head migrazioni + versione engine → restore verificato.

> **I backup contengono segreti in chiaro.** `pg_dump` include `mail.server.password`, chiavi PSP,
> ecc.: la neutralizzazione tocca l'*istanza ripristinata*, non l'artefatto. Quindi **cifratura
> envelope opzionale e consigliata** del bundle (`age`/AES-GCM, chiave dal secret manager, `key_id`
> nel manifest), indipendente dall'admin token. Se non cifrato, `dump` lo dichiara a video.

#### 1.3 CLI
```
meshble db dump    [--db-only | --full] [--encrypt] [-o snapshot.tar]
meshble db restore <snapshot.tar> [--into <db>] [--migrate] [--production-clone] [--force-clean]
meshble db verify  [<db>]            # integrità referenziale DB↔blob (vedi §1.5)
meshble blobs gc                     # vedi §1.6
```

#### 1.4 Restore — macchina a stati fail-closed
Restore è multi-stadio e **non** idempotente per natura (pg_restore, copia blob, migrazioni,
neutralize): lo rendiamo una macchina a stati tracciata.

1. **Journal**: scrive subito una riga `meshble_restore_journal(state='in_progress', …)`. Finché
   esiste, l'istanza **rifiuta di servire** ed è trattata come **neutralizzata** (fail-closed).
2. **Target**: `--into` di default ripristina in un **DB temporaneo e fa swap atomico** (rename) a
   fine successo → niente stato lacerato su crash; su DB esistente non-vuoto serve `--force-clean`
   esplicito (drop confermato). Restore è documentato come **distruttivo**.
3. **pg_restore** del `db.dump` (verificata `pg_server_version`/`archive_format` dal manifest).
4. **Migrazioni**: di default **resta all'head del dump**; se l'head ≠ quello del binario, **rifiuta**
   salvo `--migrate` esplicito (le migrazioni in avanti possono essere lossy → non automatiche).
5. **Neutralize** (vedi §3): di default **ON** sul restore; gira **dopo** le migrazioni (sullo schema
   che il binario conosce). Per un ripristino *live* servono `--production-clone` + admin token e
   `base_url` coincidente col manifest.
6. **Blob**: copia *resumable* (`exists()`→skip). Poi (§1.5) **verifica referenziale**.
7. **Commit**: journal → `done`; l'istanza diventa raggiungibile.

Un crash a qualsiasi punto lascia il journal `in_progress` ⇒ istanza non raggiungibile e neutralizzata
al riavvio, ri-eseguibile.

#### 1.5 Integrità referenziale DB↔blob
Il fast-path `--db-only` può lasciare allegati che puntano a blob assenti. Dopo ogni restore (e on
demand con `meshble db verify`): enumera `SELECT DISTINCT store, sha256 FROM meshble_attachment`,
chiama `exists()` per ciascuno, e o **fallisce** elencando gli hash mancanti, o li registra in
`meshble_missing_blob` degradando con grazia. `--db-only` **rifiuta** se mancano blob, salvo
`--allow-missing-blobs` o uno store condiviso raggiungibile dichiarato.

#### 1.6 GC dei blob orfani — safe sotto concorrenza
Dedup content-addressed ⇒ una GC naïve cancella un blob "orfano" mentre un altro upload lo sta
riusando. La GC è **mark-sweep con grace period**: cancella solo blob orfani **e** più vecchi del
massimo tempo di transazione **e** ancora non referenziati dopo la grace. In alternativa richiede la
**maintenance mode** (scritture rifiutate). Con **store condiviso da più DB**, la GC esige l'insieme
completo dei DB che lo condividono (vedi §1.8).

#### 1.7 Autenticità e autorizzazione
- I checksum rilevano corruzione, non manomissione → il **manifest è firmato** (chiave dal secret
  manager); il restore verifica la firma o dichiara esplicitamente la sorgente come fidata.
- L'**admin token** (`MESHBLE_ADMIN_TOKEN`) è **capability-scoped**: `dump` (read) ≠
  `restore` (distruttivo, esegue SQL d'archivio) ≠ `gc`. Ogni dump/restore emette un **evento di
  audit** (chi, quando, hash del manifest).

#### 1.8 Multi-tenant / store condiviso
Default: **namespace blob per-tenant** (niente dedup cross-tenant → niente existence-oracle né GC che
cancella blob di un altro DB). Se si vuole dedup cross-DB su uno store condiviso, serve una tabella di
reference che la GC consulta su **tutti** i DB. Scelta documentata, non implicita.

#### Vantaggi su Odoo
Dedup · fast-path DB-only · restore incrementale e *resumable* · restore **verificato e atomico**
(temp-db + swap, journal) · integrità referenziale · cifratura · firma · backend blob intercambiabile.

#### Decisioni aperte → vedi tabella finale (A*)

---

## 2. Configurazione istanza (l'equivalente di `odoo.conf`)

### Odoo e i suoi limiti
`odoo.conf` (INI) con **segreti in chiaro**, precedenze ambigue, **nessuna validazione**. Inoltre Odoo
mescola config di boot e impostazioni runtime (`ir.config_parameter` nel DB) senza un confine netto.

### Design Meshble

#### 2.1 Due piani distinti: boot-time vs runtime
- **Boot-time** (file/env, letti una volta all'avvio, immutabili a caldo): bind, workers, pool,
  connessione DB, JWT secret, storage backend, log. Vivono in `meshble.toml` + env.
- **Runtime** (nel DB, mutabili senza restart, **autorità unica** = DB): `base_url`, `banner`,
  `mode`, `neutralized`, feature flag, impostazioni dei moduli. È l'equivalente tipizzato di
  `ir.config_parameter`: tabella `meshble_setting(key, value, type)` + API tipizzata.

Questo elimina il conflitto "stessa cosa in due posti": una impostazione runtime cambiata via API non
viene sovrascritta da un TOML stantio al riavvio.

#### 2.2 Sorgenti e precedenza (boot-time)
`default < meshble.toml < env < flag CLI` (crate `figment`, deserializzato in struct `serde` `Config`),
validazione all'avvio (fail-fast). **`config check` è l'autorità**: fa anche i controlli cross-field e
di presenza segreti che un JSON Schema non può esprimere (lo schema generato è solo sintattico).

#### 2.3 Identità di connessione — UN modello solo
`DATABASE_URL` è l'**unica** sorgente dell'identità di connessione (host, port, dbname, user, password,
sslmode). `[database]` nel TOML porta **solo tuning non-URL** (pool_max, timeout). Se sono presenti sia
un `DATABASE_URL` completo sia campi `[database]` host/name in conflitto → **fail-fast: "ambiguous
connection config"** (mai risoluzione silenziosa). Risolve il blocker "connessione al DB sbagliato".

```toml
[instance]                  # NB: base_url/mode/neutralized sono RUNTIME (DB), non qui — vedi §2.1
name = "acme-prod"

[server]
bind = "0.0.0.0:8099"
workers = 8
proxy_mode = true

[database]                  # solo tuning; identità = env DATABASE_URL
pool_max = 10
connect_timeout = "5s"

[storage]
backend = "fs"              # fs | s3
path = "/var/lib/meshble/blobs"

[auth]
access_ttl = 900
refresh_ttl = 2592000       # jwt secret(s) via env (vedi §2.5)

[mail]
smtp_host = "smtp.acme.com"
smtp_port = 587
from = "erp@acme.com"       # smtp password via env

[modules]
load = ["base", "sales"]

[modules.sales]             # sottoalbero APERTO, validato dallo schema del modulo "sales"
default_tax = "0.22"

[log]
level = "info"
format = "json"
```

#### 2.4 Chiavi sconosciute
Sezioni **core** strict: un typo (`[serever]`, `bnid`) → l'istanza non parte. Il sottoalbero
`[modules.<name>]` è **aperto**: raccolto come `figment::Value` e validato dallo schema del modulo al
load → un modulo può avere le sue impostazioni senza rompere il boot.

#### 2.5 Segreti → solo da env / secret manager + rotazione
`DATABASE_URL`, `MESHBLE_JWT_SECRET` (+ `MESHBLE_JWT_SECRET_OLD` accettati in verifica per la
**rotazione** kid-keyed senza logout di massa), `MESHBLE_SMTP_PASSWORD`, chiavi S3,
`MESHBLE_ADMIN_TOKEN`. Presenza verificata all'avvio. `config print` redige **a livello di campo**
sulla `Config` tipizzata (di `DATABASE_URL` mostra host/db/user, **redige solo la password**) → un log
di supporto non perde il segreto né nasconde il dbname.

#### 2.6 Deployment
Un `meshble.toml` **senza segreti** è l'artefatto in **tutti** gli ambienti (anche container); env per
i segreti e per pochi override. (Niente "env-only in prod": lascerebbe `bind`/`modules.load` senza casa
in Kubernetes.)

#### Vantaggi su Odoo
Tipizzata e validata · boot vs runtime separati con autorità chiara · segreti fuori dal file con
rotazione · identità di connessione non ambigua · `config check` autoritativo · redazione corretta.

#### Decisioni aperte → tabella finale (C*)

---

## 3. Neutralized mode

### Odoo e i suoi limiti
`neutralize.sql` per modulo al restore. Limite di fondo: **muta i dati** — se il runtime non viene
neutralizzato, o si reimporta config, l'istanza torna a comunicare. Ed è **skippabile**.

### Design Meshble — gate runtime imposto + scrub dichiarativo + banner, fail-closed

#### 3.1 Stato d'istanza e segnale "neutralizzato" robusto
```
meshble_instance(id=1, name, base_url, mode, neutralized bool, banner,
                 restored_from, restored_at, updated_at)
```
`neutralized` **effettivo** = OR di più segnali, così una `UPDATE … SET neutralized=false` via psql da
sola **non basta** a riarmare l'outbound:
```
effective_neutralized =
    db.neutralized
 OR env MESHBLE_NEUTRALIZED=1
 OR file sentinella /var/lib/meshble/NEUTRALIZED
 OR (base_url corrente ≠ instance.base_url di provenienza)   // un clone si tradisce da solo
```

#### 3.2 Gate imposto al *transport*, non ai call-site
Il punto debole di Odoo ("ricordati di chiamarlo ovunque") si elimina mettendo il gate **nell'unico
canale di I/O in uscita** esposto ai moduli:
```rust
// I client grezzi (reqwest, SMTP) sono PRIVATI a meshble-core. I moduli vedono solo questi:
pub struct GatedHttp   { gate: Arc<OutboundGate>, /* raw client privato */ }
pub struct GatedMailer { gate: Arc<OutboundGate>, /* raw smtp privato  */ }
// Ogni invio consulta il gate; un modulo non ha modo di ottenere il client grezzo.
pub enum Decision { Send, SandboxSend, Sink, Block }
```
Un modulo community non può "dimenticare" il gate: non gli viene dato un client non-gated. (Più una
lint che vieta `reqwest`/SMTP diretti nei crate dei moduli.)

#### 3.3 Boot fail-closed (il momento più pericoloso)
Il primo boot di un clone pieno di lavoro arretrato (outbox con 5000 mail, cron scaduti) è il rischio
massimo. Contratto di avvio:
1. lo stato `meshble_instance` è letto **sincronamente** prima di costruire **qualsiasi** sender,
   scheduler, worker di outbox/retry;
2. riga assente o illeggibile ⇒ **neutralized=true** (Block/Sink);
3. scheduler/outbox prendono il gate come **dipendenza obbligatoria del costruttore** → non possono
   esistere senza un gate risolto. Niente worker che parte prima del gate.

#### 3.4 Politica per-canale + canali estensibili
"neutralized" (sicurezza-clone: blocca gli endpoint *reali* del mondo) è **disaccoppiato** dal `mode`
d'ambiente: uno staging permanente può usare i **suoi** endpoint sandbox. Quindi politica per-canale:
```
ChannelPolicy = Send | SandboxSend(endpoint) | Sink | Block
```
I canali sono un **registry estensibile** (non un enum chiuso): un modulo logistica può registrare
`Channel("mqtt")`. Un canale **non registrato/sconosciuto** ⇒ default **Block** (fail-closed).

#### 3.5 Scrub dichiarativo (dopo le migrazioni)
Ordine canonico: **restore → migrazioni all'head → poi** `NeutralizeAction` (scritte contro lo schema
che il binario conosce). Le azioni sono **versionate col modulo** che le possiede.
```rust
pub enum NeutralizeAction {
    ClearField      { model: &'static str, field: &'static str },
    SetField        { model: &'static str, field: &'static str, value: Value },
    DisableMatching { model: &'static str, domain: fn() -> Domain, flag_field: &'static str },
}
```
**Semantica di fallimento fail-closed**: un'azione che fallisce **interrompe il restore rumorosamente
nominando modulo+azione** (non un generico rollback). Si preferiscono flip allow-list (gli integrazioni
armate vanno a *disabilitato* di default) ai domini che potrebbero coprire solo un sottoinsieme. Lo
scrub è difesa in profondità *accanto* al gate, non al posto suo.

#### 3.6 Sink = superficie PII
Mail/webhook neutralizzati che finiscono in un dev-mailbox sono **dati reali di clienti**. Il sink ha
un contract: retention limitata, redazione opzionale di corpo/destinatari, access control, e una
modalità **"drop entirely"** per deployment sensibili. Neutralized ≠ "i dati spariscono".

#### 3.7 Restore = neutralize per default + provenienza
- `meshble db restore` **neutralizza per default** e registra `restored_from`/`restored_at`;
- live solo con `--production-clone` + admin token **e** `base_url` coincidente col manifest;
- `base_url` diverso ⇒ neutralize forzato (un clone su altro host non può fingersi prod).

#### 3.8 Banner e un-neutralize
`effective_neutralized` + `banner` esposti nell'UI-contract agnostico (o `/api/instance`) → il frontend
mostra la fascia. La **promozione** di una copia DR a prod è un bisogno reale ma lo scrub è
irreversibile: esiste un comando esplicito **`meshble instance unneutralize`** (admin token) che (a)
richiede di **re-fornire i segreti** cancellati / riconfigurare i provider, (b) emette audit, (c) è
l'**unico writer sancito** di `neutralized=false`.

#### Vantaggi su Odoo
Neutralizzazione **imposta dal transport** e **fail-closed al boot** (non solo dai dati) · segnale
robusto non disarmabile da una UPDATE · politica per-canale e canali estensibili · ordine
restore→migrazione→scrub corretto · un-neutralize controllato.

---

## Riepilogo decisioni

| Id | Tema | Decisione |
|----|------|-----------|
| A1 | blob store default | `FsBlobStore` content-addressed (pluggable S3); `DbBlobStore` fuori dalla v1 |
| A2 | trait blob | **streaming** (AsyncRead) + `put` verifica `sha256(bytes)==hash` |
| A3 | identità admin | `MESHBLE_ADMIN_TOKEN` da env, **capability-scoped** (dump/restore/gc) + audit |
| A4 | restore target | temp-db + **swap atomico** di default; DB esistente solo con `--force-clean` |
| A5 | restore atomicità | **journal** fail-closed (istanza neutralizzata finché incompleto); blob resumable |
| A6 | migrazioni su restore | **opt-in `--migrate`**; default resta all'head del dump, rifiuta mismatch |
| A7 | integrità blob | pass referenziale post-restore + `meshble db verify`; `--db-only` rifiuta blob mancanti |
| A8 | GC | mark-sweep con grace period **o** maintenance mode; multi-DB esige tutti i DB |
| A9 | sicurezza artefatto | cifratura envelope opzionale (`age`) + **firma** del manifest |
| A10 | multi-tenant | default namespace per-tenant; dedup cross-DB solo con reference table esplicita |
| C1 | formato config | `meshble.toml` + env + `figment`, validata; `config check` autoritativo |
| C2 | boot vs runtime | boot=file/env (immutabile); runtime=DB (`meshble_setting`, autorità unica) |
| C3 | connessione | `DATABASE_URL` unica identità; `[database]` solo tuning; conflitto ⇒ fail-fast |
| C4 | chiavi ignote | core strict; `[modules.<name>]` aperto, validato dal modulo |
| C5 | segreti | solo env; rotazione JWT kid-keyed (primario + vecchi accettati); redazione per-campo |
| C6 | deployment | `meshble.toml` senza segreti in tutti gli ambienti + env per segreti/override |
| N1 | gate | imposto al **transport** (GatedHttp/GatedMailer), client grezzi privati |
| N2 | boot | fail-closed: stato letto prima dei worker; assente ⇒ neutralized; gate obbligatorio |
| N3 | segnale | `effective = OR(db, env, sentinella, base_url-mismatch)` |
| N4 | canali | registry **estensibile**; sconosciuto ⇒ Block; politica per-canale (Send/Sandbox/Sink/Block) |
| N5 | neutralized vs mode | **disaccoppiati** (clone-safety ≠ ambiente) |
| N6 | scrub | dopo le migrazioni; versionato col modulo; fallimento fail-closed e nominato |
| N7 | sink | contract PII (retention, redazione, drop-entirely) |
| N8 | restore | neutralize **per default** + provenienza; live solo con `--production-clone` + token + base_url match |
| N9 | un-neutralize | comando esplicito admin, re-fornisce segreti, audit, unico writer di `false` |
