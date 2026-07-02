# Panoramica e quickstart

Kigumi è un framework applicativo **headless** e **schema-driven** scritto in Rust: una sola **definizione di modello** è l'unica sorgente di verità, e da essa il framework **genera** lo schema Postgres, l'API REST, la specifica OpenAPI, il **contratto-UI** agnostico in JSON e la policy di security (ACL e record rule). Il core non impone né un frontend né un protocollo applicativo: tutto è esposto tramite standard generati a partire dallo schema. La composizione dei moduli è **risolta e verificata a compile time** — i moduli disponibili in un binario sono quelli i cui crate sono linkati, e il loro grafo di dipendenze è validato prima ancora di toccare il database. Questa pagina spiega cos'è Kigumi, ne descrive l'architettura e fornisce un quickstart completo per portare un'istanza dallo zero a un'API REST autenticata, con un'app web di riferimento.

## Cosa è Kigumi

Il principio centrale è **un modello, una sorgente di verità**. Si descrive un'entità una volta sola, in Rust, con la macro `#[model]`; da quella descrizione (`ModelDescriptor`) il framework proietta:

| Artefatto generato | Crate responsabile | A che serve |
|---|---|---|
| Schema Postgres (DDL + migrazioni versionate) | `kigumi-schema` / `kigumi-db` | tabelle, colonne, FK, vincoli |
| API REST headless | `kigumi-server` | CRUD sicuro e azioni dal catalogo |
| Specifica OpenAPI 3.1 | `kigumi-schema` | `GET /openapi.json` |
| Contratto-UI agnostico (JSON) | `kigumi-schema` | un frontend generico disegna form e tabelle *dal contratto* |
| Policy di security (ACL + record rule) | `kigumi-core` | accesso imposto dal percorso di codice, non solo dai dati |

Caratteristiche portanti:

- **Agnostico**: il core non sa nulla del frontend né del trasporto; l'app web è solo un consumatore del contratto-UI e dell'API REST.
- **Composizione a compile time**: un modulo è un crate Rust che auto-registra i propri modelli, ACL e record rule nel **registro a compile time** (tramite `inventory`). Solo i moduli linkati nel binario sono disponibili; il grafo delle dipendenze è validato da `resolve_modules`, con controllo di compatibilità SemVer verso la versione del framework (`check_compat` / `FRAMEWORK_VERSION`).
- **Multi-azienda**: `res.company` è l'unità di isolamento dei dati; lo scoping per azienda è applicato dalla security layer (`Ctx` con la company attiva più una record rule).
- **Fail-fast sui segreti**: l'istanza si rifiuta di partire se manca un segreto richiesto (`DATABASE_URL`, `KIGUMI_JWT_SECRET`).

## Mappa dell'architettura

Il workspace Cargo (`Cargo.toml`, `members = ["crates/*", "modules/*", "apps/*"]`) è organizzato in tre livelli: i **crate** del framework, i **moduli** di business e le **app** eseguibili. Esiste inoltre un'app web di riferimento sotto `web/`.

### Crate (`crates/`)

| Crate | Ruolo |
|---|---|
| `kigumi-core` | metamodello ispezionabile, domini AST, security (ACL + record rule + sudo), registro a compile time, versioning |
| `kigumi-macros` | proc-macro `#[model]` / `#[extend]` |
| `kigumi-schema` | proiezioni: DDL Postgres, contratto-UI JSON, OpenAPI 3.1 |
| `kigumi-db` | persistenza Postgres (sqlx): CRUD security-enforced + migrazioni versionate |
| `kigumi-auth` | auth JWT HS256 (Bearer → `Ctx` fidato), hashing password |
| `kigumi-config` | configurazione boot-time tipizzata e validata + lettura segreti dall'ambiente |
| `kigumi-storage` | blob store content-addressed dietro un trait (`BlobStore`) per gli allegati |
| `kigumi-server` | server axum headless: metadata + CRUD dal catalogo |
| `kigumi` | facade (prelude): un modulo dipende solo da questo crate |

Il prelude pubblico è in `crates/kigumi/src/lib.rs`: un modulo apre la propria definizione con

```rust
use kigumi::prelude::*;
```

e riceve tutto il necessario — `#[model]`, `#[extend]`, `Ctx`, `Domain`, `Model`, `ModelDescriptor`, `ModuleManifest`, `ModuleDep`, i tipi della security (`Acl`, `RecordRule`) e i registratori esposti come macro dal crate `kigumi` (`register_module!`, `register_acls!`, `register_rules!`, `register_action!`, e simili).

### Moduli (`modules/`)

Ogni modulo dichiara un `MANIFEST` (`ModuleManifest`) con nome, versione, range di compatibilità con il framework (campo `framework`) e dipendenze (`depends`), e si auto-registra con `kigumi::register_module!(MANIFEST)`.

| Modulo | Versione | Dipende da | Summary (dal manifest) |
|---|---|---|---|
| `base` | `1.0.0` | — | Foundational models: currency, partner, company |
| `mail` | `1.0.0` | `base` | Headless chatter: messages, tracking, followers, activities |
| `sales` | `1.0.0` | `base`, `mail` | Sales order management |
| `account` | `1.0.0` | `base`, `mail` | Double-entry general ledger |
| `stock` | `1.0.0` | `base`, `sales`, `mail` | Inventory — locations, quants, pickings and moves |

`base` è la radice del grafo (nessuna dipendenza) e non può essere disinstallato. Tutti i moduli dichiarano `framework: ">=0.1, <0.2"`. Le dipendenze nella tabella sono quelle dichiarate nel `MANIFEST`; installare un modulo ne risolve la **chiusura transitiva** (ad esempio installare `stock` tira dentro `sales`, e quindi `base` e `mail`).

### App (`apps/`)

| App | Binario | Ruolo |
|---|---|---|
| `kigumi-cli` | `kigumi` | la CLI unica per operare un'istanza: `serve`, `migrate`, `config`, `user`, `acl`, `rule`, `module`, `version` |
| `renderer-demo` | `kigumi-renderer-demo` | demo eseguibile: migra+seeda un modello e serve API + renderer di riferimento |

La CLI `kigumi` linka i crate dei moduli (`kigumi-mod-base`, `kigumi-mod-mail`, `kigumi-mod-sales`, `kigumi-mod-account`, `kigumi-mod-stock`); proprio perché linkati, le loro registrazioni `inventory` sono presenti nel binario e i moduli risultano *disponibili* all'installazione. Solo i moduli **installati** vengono però migrati e serviti.

### Web (`web/`)

`web/` è una SPA Vite/React: l'app web di riferimento per l'admin UI. In sviluppo gira come processo separato sulla porta `5180` e fa da proxy delle path `/api`, `/auth` e `/openapi.json` verso `kigumi serve` (default `127.0.0.1:8099`), così il browser resta same-origin (nessun CORS). Il server Rust è headless e **non** serve asset statici: l'app web è un client separato del contratto-UI e dell'API REST.

## Quickstart

Questo percorso porta un'istanza dallo zero a un'API REST autenticata, con un modulo di business installato e l'app web di riferimento aperta.

### Prerequisiti

- **Toolchain Rust** (stable, edition 2021). Installa con [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **PostgreSQL** in esecuzione e raggiungibile, con i comandi client `createdb`/`psql` disponibili.
- **Node.js** + npm (solo se vuoi aprire l'app web di riferimento sotto `web/`).

### 1. Compila il binario `kigumi`

Dalla radice del workspace:

```bash
cargo build --release -p kigumi-cli
```

Il binario prodotto è `target/release/kigumi`. Negli esempi seguenti puoi usare `cargo run -p kigumi-cli -- <comando>` durante lo sviluppo, oppure `kigumi <comando>` se il binario è nel `PATH`.

### 2. Crea il database

`kigumi` si connette a un database **già esistente** (non lo crea): crealo a parte.

```bash
createdb kigumi
```

### 3. Configura i segreti (env)

I segreti si leggono **solo** dall'ambiente, mai dal file di configurazione; l'istanza fa fail-fast se ne manca uno richiesto. Sono richiesti `DATABASE_URL` (un DSN Postgres completo: l'unica sorgente dell'identità di connessione) e `KIGUMI_JWT_SECRET` (il segreto HS256 di firma dei token).

```bash
export DATABASE_URL="postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi"
export KIGUMI_JWT_SECRET="$(openssl rand -hex 32)"
```

`DATABASE_URL` deve essere un URL `postgres://` valido, altrimenti la validazione dei segreti fallisce all'avvio.

### 4. Prepara `kigumi.toml`

Le impostazioni boot-time NON segrete vivono in `kigumi.toml` (default: `./kigumi.toml`, oppure `$KIGUMI_CONFIG`, oppure `--config <path>`). La configurazione effettiva è data dalla stratificazione `defaults < kigumi.toml < variabili d'ambiente con prefisso KIGUMI_CONF_` (nidificate con `__`, ad esempio `KIGUMI_CONF_SERVER__BIND=0.0.0.0:9000`). Il file è opzionale per i comandi che non avviano il blob store, ma `serve` richiede `storage.path` quando lo storage backend è `fs` (il default): senza, la validazione fallisce con fail-fast. Parti dall'esempio:

```bash
cp kigumi.toml.example kigumi.toml
```

Un `kigumi.toml` minimale per il quickstart:

```toml
[server]
bind = "127.0.0.1:8099"

[storage]
backend = "fs"
path = "/var/lib/kigumi/blobs"
```

> In v1 è implementato il solo backend di storage `fs` (`FsBlobStore`, file content-addressed). Il valore `s3` è previsto dallo schema di configurazione ma non ancora realizzato.

Verifica la configurazione effettiva (segreti redatti) con:

```bash
kigumi config check
kigumi config print
```

`config print` stampa la configurazione effettiva con i segreti mascherati e, in coda, le runtime settings lette dal database.

### 5. Migra il catalogo

```bash
kigumi migrate
```

`migrate` assicura gli schemi del framework (auth, sequenze, settings, accessi, moduli) e, su un database fresco, installa automaticamente solo `base` (più la sua chiusura di dipendenze). Gli altri moduli sono opt-in. La migrazione crea le tabelle dei modelli dei moduli installati in ordine di dipendenza delle FK e seeda i dati di riferimento di `base` (una valuta `EUR` e una company di default).

### 6. Installa un modulo di business

I moduli disponibili sono solo quelli linkati nel binario; l'installazione risolve la **chiusura di dipendenze** (dipendenze prima dei dipendenti) e ne migra le tabelle, in modo idempotente. Elenca i moduli e installa `sales`:

```bash
kigumi module list
kigumi module install sales
```

Installare `sales` tira dentro anche `mail` (sua dipendenza, oltre a `base` già presente). L'output di `module list` mostra per ciascun modulo nome, versione, stato (`installed`/`available`) e summary.

### 7. Effettua il bootstrap dell'admin

Su un'istanza fresca, l'utente `admin` viene creato dalla variabile `KIGUMI_ADMIN_PASSWORD` (nessuna password è hardcoded). Il bootstrap avviene dentro `serve`: se non esiste già un utente `admin` e la variabile è impostata, viene creato con tutti i gruppi dichiarati dalle ACL/record rule dei moduli linkati più i gruppi base `user`/`admin`, e assegnato a ogni company esistente.

```bash
export KIGUMI_ADMIN_PASSWORD="$(openssl rand -base64 24)"
```

> Se `KIGUMI_ADMIN_PASSWORD` non è impostata, `serve` parte comunque ma stampa un avviso e nessun admin viene creato.

### 8. Avvia il server

```bash
kigumi serve
```

`serve` esegue in sequenza: `migrate` → bootstrap dell'admin → avvio del server axum. All'avvio stampa l'URL di ascolto e il numero di modelli registrati nel binario, ad esempio:

```
kigumi serving on http://127.0.0.1:8099  (N models)
```

> Il numero stampato è il totale dei modelli **registrati** (cioè di tutti i moduli linkati). L'API serve però soltanto i modelli dei moduli **installati**: un modello il cui modulo non è installato non compare nel catalogo esposto dal router. L'accesso effettivo è l'unione tra la baseline compilata e gli eventuali override runtime (ACL e record rule) presenti nel database.

### 9. Chiama l'API REST con curl

Prima ottieni un access token con login (`POST /auth/login`, body `{login, password}`); la risposta è `{ access_token, refresh_token, token_type, expires_in }` con `token_type` pari a `"Bearer"`. Poi usa il token come `Authorization: Bearer` sulle route dati.

```bash
# 1) login → estrai l'access token
TOKEN=$(curl -s -X POST http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$KIGUMI_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')

# 2) lista record di un modello (envelope: data/total/limit/offset)
curl -s http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN"
```

La lista risponde con un envelope `{ "data": [...], "total": N, "limit": ..., "offset": ... }`. Altre route utili, tutte montate dal catalogo:

| Route | Metodo | Cosa fa |
|---|---|---|
| `/openapi.json` | GET | specifica OpenAPI 3.1 generata |
| `/api/models` | GET | elenco dei modelli serviti |
| `/api/:name/view` | GET | contratto-UI del modello (form + tabella) |
| `/api/:name` | GET / POST | lista (paginata) / crea |
| `/api/:name/:id` | GET / PATCH / DELETE | leggi / aggiorna / elimina un record |
| `/api/:name/:id/action/:action` | POST | esegue un'azione di transizione di stato registrata |
| `/auth/login`, `/auth/refresh`, `/auth/logout`, `/auth/me` | — | flusso di autenticazione JWT |

### 10. Apri l'app web di riferimento

L'app web sotto `web/` gira come processo separato e, in sviluppo, fa da proxy verso il server in esecuzione:

```bash
cd web
npm install
npm run dev      # http://localhost:5180
```

Con `kigumi serve` attivo su `127.0.0.1:8099`, le path `/api`, `/auth` e `/openapi.json` vengono proxate verso il backend, quindi le chiamate dell'app web raggiungono l'API reale dallo stesso origin.

## Indice della guida

| Pagina | Contenuto |
|---|---|
| [README.md](README.md) | Questa pagina: panoramica del framework e quickstart end-to-end. |
| [architettura.md](architettura.md) | Architettura in dettaglio: crate, metamodello, generazione degli artefatti, flusso di composizione a compile time. |
| [installazione.md](installazione.md) | Installazione completa: prerequisiti, build, creazione del database, prima esecuzione. |
| [configurazione.md](configurazione.md) | Configurazione: `kigumi.toml`, segreti via ambiente, override `KIGUMI_CONF_*`, settings runtime nel database. |
| [moduli.md](moduli.md) | I moduli inclusi (`base`, `mail`, `sales`, `account`, `stock`): modelli esposti, dipendenze, install/uninstall. |
| [moduli-custom.md](moduli-custom.md) | Come scrivere un modulo: `#[model]`, manifest, ACL, record rule, azioni, registrazione nel catalogo. |
| [api.md](api.md) | L'API REST e OpenAPI: route, envelope di risposta, contratto-UI, autenticazione JWT. |
| [sicurezza.md](sicurezza.md) | Modello di security: ACL, record rule, gruppi, sudo, multi-azienda, override runtime additivi. |
