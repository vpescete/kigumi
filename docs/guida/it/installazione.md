# Installazione su ambiente

Questa guida descrive l'installazione e il deploy di un'istanza Kigumi, dal codice sorgente fino a un server in produzione. Kigumi è un framework applicativo headless e schema-driven scritto in Rust: si compila in un singolo binario (`kigumi`) che migra il catalogo, applica la security e serve l'API. Il percorso è sempre lo stesso: si compila il binario, si predispone un database PostgreSQL, si forniscono i segreti via ambiente, si scrive il file di configurazione non-segreto, e infine si porta su l'istanza con la sequenza `migrate` → `module install` → `serve`. Per una panoramica del prodotto vedi [README.md](README.md); per l'architettura [architettura.md](architettura.md); per il dettaglio di ogni chiave di configurazione [configurazione.md](configurazione.md).

## Prerequisiti

| Componente | Requisito |
|---|---|
| Toolchain Rust | Toolchain stabile recente. Il workspace fissa `edition = "2021"`. Installa via [rustup](https://rustup.rs). |
| PostgreSQL | Un server PostgreSQL raggiungibile e già avviato; `DATABASE_URL` è l'unica identità di connessione. I comandi client `createdb`/`psql` tornano utili per creare il database. |
| Node.js + npm | Solo per il frontend web opzionale in `web/` (Vite 5, React 18). |

> Il workspace dichiara `edition = "2021"` ma non fissa una `rust-version` (MSRV) esplicita; usa una toolchain stabile recente. Vedi le **Incertezze** in coda.

## Creare un'applicazione (`kigumi new`)

Il percorso raccomandato per costruire il tuo verticale:

```bash
cargo install kigumi-cli
kigumi new mioshop           # chiede quali moduli extra includere (sales, account, stock)
cd mioshop
createdb mioshop
export DATABASE_URL=postgres://localhost/mioshop
export KIGUMI_JWT_SECRET=cambiami
KIGUMI_ADMIN_PASSWORD=cambiami cargo run -p app -- migrate
cargo run -p app -- serve    # http://127.0.0.1:8600 (override con KIGUMI_BIND)
```

Il workspace contiene un crate modulo (un modello ticket di partenza — vedi [moduli-custom.md](moduli-custom.md)), un binario server di ~45 righe su `kigumi-runtime` — che possiede il wiring operativo: schemi del framework, installazione moduli con replay delle migrazioni dati, seeding dei dati di riferimento, bootstrap dell'admin, i worker cron/job e il server a catalogo statico — più il kit per gli agenti: `AGENTS.md`/`CLAUDE.md`, una skill e un agente Claude Code locali al progetto, e il comando `app mcp <login>` che serve l'app sul Model Context Protocol (vedi [api.md](api.md#mcp-la-superficie-ai)). `migrate` è idempotente — eseguilo a ogni deploy; applica anche gli step `register_migration!` pendenti.

Il resto di questa pagina copre l'operatività del repository del framework stesso (la CLI `kigumi` completa con il suo file di configurazione, l'installazione dinamica dei moduli e la SPA di amministrazione).

## Ottenere il sorgente e compilare

Clona il repository e compila il workspace in modalità release:

```bash
git clone https://github.com/vpescete/kigumi
cd kigumi
cargo build --release
```

Il binario operativo si chiama `kigumi` ed è prodotto dal crate `kigumi-cli`, che dichiara esplicitamente il nome del bin nel proprio `Cargo.toml`:

```toml
[[bin]]
name = "kigumi"
path = "src/main.rs"
```

`cargo build --release` compila l'intero workspace. Per compilare solo la CLI puoi restringere il target al pacchetto:

```bash
cargo build --release -p kigumi-cli
```

In entrambi i casi, al termine il binario si trova in:

```
target/release/kigumi
```

Tutti i comandi seguenti usano questo binario. Negli esempi è abbreviato in `kigumi`; in un ambiente reale invocalo con il percorso completo `target/release/kigumi`, oppure copialo in una directory nel `PATH`. Durante lo sviluppo puoi anche usare `cargo run -p kigumi-cli -- <comando>`.

I moduli applicativi (`base`, `mail`, `sales`, `account`, `stock`) sono linkati staticamente nel binario tramite le rispettive crate: i loro modelli, ACL e record rule si auto-registrano nel registro a compile time (via `inventory`). Solo i moduli la cui crate è linkata nel binario sono disponibili per l'installazione a runtime — vedi [moduli.md](moduli.md).

Verifica versione del framework e moduli linkati:

```bash
kigumi version
```

Il comando stampa la versione del framework e, riga per riga, i moduli linkati con la loro versione (es. `module base 2.0.0`).

## Predisposizione del database

`kigumi` si connette a un database **già esistente** — non lo crea. Crealo a parte:

```bash
createdb kigumi
```

Non serve eseguire DDL a mano: `kigumi migrate` (e `kigumi serve`) creano e versionano tutti gli schemi. È sufficiente che il database esista e che l'utente in `DATABASE_URL` possa crearvi tabelle.

### Formato del DSN `DATABASE_URL`

`DATABASE_URL` è un DSN PostgreSQL completo ed è **l'unica** sorgente dell'identità di connessione (host, porta, database, utente, password, sslmode). Il valore viene validato all'avvio: deve essere un URL con schema `postgres://` oppure `postgresql://`, altrimenti l'istanza rifiuta di partire con `"DATABASE_URL is not a valid postgres:// URL"`.

```bash
# forma generale
DATABASE_URL=postgres://UTENTE:PASSWORD@HOST:PORTA/NOME_DB

# esempio
DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
```

La sezione `[database]` del file di configurazione contiene **solo** parametri di tuning (`pool_max`, `connect_timeout`): non esiste alcun campo `host`/`name` lì, proprio per evitare sovrapposizioni ambigue con il DSN. Inserire un `host` in `[database]` è una chiave sconosciuta e provoca fail-fast.

## Segreti e variabili d'ambiente richieste

I segreti **non** vanno mai nel file `kigumi.toml`: si forniscono esclusivamente dall'ambiente (o da un secret manager) e la loro presenza è verificata all'avvio. L'istanza fa fail-fast se manca un segreto obbligatorio.

| Variabile | Obbligatorietà | Scopo |
|---|---|---|
| `DATABASE_URL` | **Obbligatoria** | DSN PostgreSQL completo: identità di connessione. |
| `KIGUMI_JWT_SECRET` | **Obbligatoria** | Segreto di firma HS256 per i token di accesso/refresh. |
| `KIGUMI_ADMIN_PASSWORD` | Per il bootstrap | Password dell'utente `admin` creato al primo `serve` (vedi sotto). |
| `KIGUMI_SMTP_PASSWORD` | Condizionale | Obbligatoria **solo** se `[mail].smtp_host` è configurato; altrimenti il caricamento dei `Settings` fallisce. |
| `KIGUMI_JWT_SECRET_OLD` | Opzionale | Segreto JWT precedente, **riservato** alla rotazione: in v1 viene caricato in configurazione ma non ancora passato al verificatore (l'`Authenticator` riceve il solo `KIGUMI_JWT_SECRET`). |
| `KIGUMI_ADMIN_TOKEN` | Opzionale | Segreto **riservato** alla futura protezione delle operazioni distruttive sul db (dump/restore/gc): in v1 viene caricato ma l'enforcement non è ancora cablato (gli endpoint non esistono ancora). |
| `KIGUMI_NEW_PASSWORD` | Opzionale | Password per `kigumi user create` / `set-password` quando non si passa `--password`. |

`DATABASE_URL` e `KIGUMI_JWT_SECRET` sono le due variabili **richieste** in assoluto: la lettura dell'ambiente fallisce se una di esse manca o è vuota. La verifica incrociata SMTP è esplicita: se in configurazione è presente `mail.smtp_host` ma manca `KIGUMI_SMTP_PASSWORD`, il caricamento dei `Settings` ritorna l'errore `"mail.smtp_host is set but KIGUMI_SMTP_PASSWORD is not"`.

Il file `.env.example` nel repository elenca i segreti come template. Esempio minimo per partire:

```bash
export DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
export KIGUMI_JWT_SECRET="$(openssl rand -hex 32)"
export KIGUMI_ADMIN_PASSWORD="$(openssl rand -base64 24)"
```

Per ispezionare la configurazione effettiva con i segreti redatti (la password del DSN è mascherata, host/db/utente restano visibili; gli altri segreti compaiono come `set (****)` / `unset`):

```bash
kigumi config check    # valida la configurazione effettiva
kigumi config print    # stampa la config redatta + i runtime setting dal db
```

## Il file di configurazione

Le impostazioni non-segrete (boot-time) vivono in un file TOML. Copia il template fornito e adattalo:

```bash
cp kigumi.toml.example kigumi.toml
```

### Risoluzione del percorso del file

Il binario risolve il percorso del file in quest'ordine:

1. il flag globale `--config <path>` (disponibile su ogni sottocomando);
2. la variabile d'ambiente `KIGUMI_CONFIG`;
3. il default `./kigumi.toml` nella directory corrente.

```bash
kigumi --config /etc/kigumi/kigumi.toml serve
# oppure
export KIGUMI_CONFIG=/etc/kigumi/kigumi.toml
kigumi serve
```

### Layering e override da ambiente

La configurazione si compone a strati, dal meno al più prioritario:

```
defaults < kigumi.toml < variabili d'ambiente KIGUMI_CONF_*
```

Le variabili `KIGUMI_CONF_*` mappano sulle sezioni del file usando il **doppio underscore** `__` come separatore di nidificazione. Il prefisso `KIGUMI_CONF_` è volutamente distinto dalle variabili dei segreti (`DATABASE_URL`, `KIGUMI_JWT_SECRET`, …), così i segreti non vengono mai catturati dal layer di configurazione.

```bash
# equivale a [server] bind = "0.0.0.0:9000"
export KIGUMI_CONF_SERVER__BIND=0.0.0.0:9000
```

La validazione è fail-fast: una sezione o una chiave sconosciuta in una sezione core fa rifiutare l'avvio (invece di ignorare silenziosamente i typo). I sottoalberi `[modules.<nome>]` sono invece **aperti**: vengono catturati verbatim e validati dal modulo proprietario.

### Chiavi principali del template

```toml
[instance]
name = "acme-prod"

[server]
bind = "0.0.0.0:8099"          # host:port su cui il server ascolta (validato come SocketAddr)
workers = 8
proxy_mode = true              # vedi note di produzione

[database]                     # SOLO tuning — l'identità è il DSN DATABASE_URL
pool_max = 10
connect_timeout = "5s"

[storage]
backend = "fs"                 # fs | s3
path = "/var/lib/kigumi/blobs"

[auth]
access_ttl = 900               # 15 min (segreto JWT via env KIGUMI_JWT_SECRET)
refresh_ttl = 2592000          # 30 giorni

[mail]
smtp_host = "smtp.acme.com"    # password SMTP via env KIGUMI_SMTP_PASSWORD
smtp_port = 587
from = "erp@acme.com"
```

Due chiavi sono load-bearing per la messa in funzione:

- **`server.bind`** — l'indirizzo `host:port` su cui il server ascolta. Deve essere un `host:port` valido (default `127.0.0.1:8099`); un valore non parsabile come socket fa fallire la validazione con `"server.bind is not a host:port"`.
- **`storage.path`** — la radice del blob store su filesystem. Con `backend = "fs"` (il default) `storage.path` è **obbligatorio**: la validazione rifiuta `backend = fs` senza `path` (`"storage.backend = fs requires storage.path"`), e `serve` ritorna `"storage.path is required for the fs blob store"` se manca. Con `backend = "s3"` è invece `storage.bucket` a essere richiesto dalla validazione (`"storage.backend = s3 requires storage.bucket"`).

Per il dettaglio completo di ogni chiave vedi [configurazione.md](configurazione.md).

## Sequenza di messa in funzione

L'avvio di un'istanza fresca segue tre passi. Ognuno è idempotente e può essere rieseguito.

### 1. `kigumi migrate`

```bash
kigumi migrate
```

Assicura gli schemi del framework (auth, sequenze, settings, accessi, moduli) e poi migra i modelli dei moduli **installati**. Su un database davvero fresco non c'è ancora nessun modulo installato: in quel caso `migrate` installa automaticamente solo `base` e la sua chiusura delle dipendenze (gli altri moduli sono opt-in). Migra le loro tabelle in ordine di dipendenza FK, crea le tabelle di relazione Many2many in una seconda passata, e — se `base` è installato — semina i dati di riferimento minimi (una valuta `EUR` e una `Main Company` di default, più la lista read-only dei gruppi referenziati da ACL/record rule). Semina inoltre i runtime setting di default (`base_url`, `mode = production`) senza mai sovrascrivere una modifica dell'operatore.

> Nota di upgrade: se il database è stato migrato **prima** che esistesse la selezione dei moduli (la sua anagrafica per-modello ha già righe), `migrate` mantiene **tutti** i moduli che aveva, così l'aggiornamento non nasconde silenziosamente modelli prima disponibili.

### 2. `kigumi module install <NAME>`

```bash
kigumi module install sales
```

Installa un modulo **e la sua chiusura delle dipendenze** (le dipendenze prima), poi migra le loro tabelle (idempotente). I moduli già installati vengono saltati. Installare `sales`, per esempio, tira dentro anche `mail` (oltre a `base` già presente). Comandi correlati:

```bash
kigumi module list               # elenca i moduli linkati con versione, stato e summary
kigumi module install account    # installa account + dipendenze, poi migra
kigumi module uninstall sales    # smette di migrare/servire il modulo; tabelle e dati restano
```

`base` non può essere disinstallato. La disinstallazione di un modulo è rifiutata se un altro modulo installato dipende ancora da esso. Per il modello dei moduli vedi [moduli.md](moduli.md) e per i moduli custom [moduli-custom.md](moduli-custom.md).

### 3. `kigumi serve`

```bash
kigumi serve
```

`serve` esegue tre cose in sequenza e poi resta in ascolto:

1. **ri-migra** i moduli installati (richiama internamente lo stesso `migrate`), così avviare il server allinea sempre lo schema;
2. **fa il bootstrap dell'admin** da `KIGUMI_ADMIN_PASSWORD` (vedi sotto);
3. **serve** l'API sicura su `server.bind`, esponendo solo i modelli dei moduli installati.

All'avvio il server unisce la baseline di ACL/record rule compilata nel binario con eventuali override runtime presenti nel database, avvia uno scheduler in background per i cron job registrati, e inizializza il blob store su filesystem dalla radice `storage.path`. Stampa l'URL su cui sta servendo e il numero di modelli esposti:

```
kigumi serving on http://127.0.0.1:8099  (N models)
```

Le rotte principali esposte includono `/openapi.json`, `/api/models`, `/api/:name/view`, il CRUD `/api/:name` e `/api/:name/:id`, l'autenticazione `/auth/login` · `/auth/refresh` · `/auth/logout` · `/auth/me`, e gli health-check `/health` · `/ready`. Per l'API e la security vedi [api.md](api.md) e [sicurezza.md](sicurezza.md).

### Sequenza completa di esempio

```bash
export DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
export KIGUMI_JWT_SECRET="$(openssl rand -hex 32)"
export KIGUMI_ADMIN_PASSWORD="$(openssl rand -base64 24)"
export KIGUMI_CONFIG=/etc/kigumi/kigumi.toml

createdb kigumi
kigumi migrate                 # schemi framework + base + sua chiusura
kigumi module install sales    # moduli applicativi desiderati (+ dipendenze)
kigumi serve                   # ri-migra, bootstrap admin, serve
```

## Bootstrap dell'admin

Al primo `serve`, se non esiste già un utente `admin`, il binario lo crea a partire da `KIGUMI_ADMIN_PASSWORD` (la password non è mai hardcoded). Se `KIGUMI_ADMIN_PASSWORD` non è impostata, il bootstrap viene saltato con l'avviso `"warning: no admin user; set KIGUMI_ADMIN_PASSWORD to bootstrap one"` e nessun admin viene creato.

L'admin bootstrappato riceve **tutti** i gruppi dichiarati dai moduli linkati (via ACL/record rule) più i gruppi base `user` e `admin`, e viene assegnato a tutte le aziende esistenti come scope multi-azienda (con la prima come azienda attiva), così un'istanza appena creata può operare ogni modulo. La password viene salvata come hash (argon2). Le aziende create in seguito vanno concesse esplicitamente all'admin.

In alternativa puoi gestire gli utenti via CLI senza passare dal bootstrap automatico:

```bash
kigumi user create alice --password 's3cret' --groups user,sales
kigumi user set-password alice --password 'nuova'
kigumi user grant alice admin
kigumi user company alice --active 1 --allowed 1,2
```

La password può anche arrivare da `KIGUMI_NEW_PASSWORD` invece che da `--password`.

## Frontend web

Il frontend di amministrazione è una SPA React/Vite opzionale in `web/`. In sviluppo è un processo separato dal server Rust e instrada le chiamate API verso un'istanza `kigumi serve` viva.

```bash
cd web
npm install
npm run dev        # server di sviluppo su http://localhost:5180
```

Il dev server di Vite ascolta sulla porta **5180** e fa da proxy verso il `kigumi serve` in esecuzione (default `127.0.0.1:8099`), così il browser resta same-origin e non serve CORS. I path inoltrati sono:

```ts
proxy: {
  '/api': 'http://127.0.0.1:8099',
  '/auth': 'http://127.0.0.1:8099',
  '/openapi.json': 'http://127.0.0.1:8099',
}
```

Quindi in sviluppo avvia in parallelo il backend (`kigumi serve`) e il frontend (`npm run dev`).

Per la build di produzione:

```bash
cd web
npm run build      # produce gli asset statici in web/dist/
```

Gli asset statici prodotti in `web/dist/` vanno serviti da un web server / reverse proxy, instradando `/api`, `/auth` e `/openapi.json` verso il backend Kigumi. Il server Rust è headless e **non** serve asset statici.

> Nota: la SPA in `web/` gira di default su dati mock in-memory (mockup navigabili del design-system) e non richiede il backend per essere navigata; il proxy verso `kigumi serve` serve quando la si collega all'API reale. Altri comandi disponibili: `npm run preview` (anteprima della build) e `npm run typecheck` (`tsc --noEmit`).

## Note di produzione

### Reverse proxy e `server.proxy_mode`

In produzione metti l'istanza dietro un reverse proxy (TLS, header di forwarding). La chiave `[server] proxy_mode = true` è prevista per fidarsi degli header `X-Forwarded-*` quando si è dietro un reverse proxy. Imposta `server.bind` di conseguenza (es. `0.0.0.0:8099` per ascoltare su tutte le interfacce dietro il proxy, oppure un indirizzo interno).

### Worker

`[server] workers` esprime il numero di worker desiderato. Il runtime async usa lo scheduler multi-thread di Tokio.

### Backend di storage: `fs` vs `s3`

`[storage] backend` accetta `fs` o `s3`:

- **`fs`** (default): blob store content-addressed su filesystem, con radice `storage.path` (obbligatoria). I byte identici deduplicano in un unico file immutabile. Questo è il backend attualmente implementato e quello che `serve` istanzia (`FsBlobStore`).
- **`s3`**: la validazione della configurazione richiede `storage.bucket` (e si possono indicare `region`, con le credenziali via ambiente). Nota però che a livello di storage, in v1, è disponibile solo il backend su filesystem: la crate `kigumi-storage` documenta `S3BlobStore` come fuori da v1. Vedi le **Incertezze**.

### Rotazione del segreto JWT

Il design prevede `KIGUMI_JWT_SECRET_OLD` per ruotare il segreto di firma JWT senza invalidare i token già emessi: si imposta `KIGUMI_JWT_SECRET` al nuovo valore e `KIGUMI_JWT_SECRET_OLD` al precedente, da mantenere accettato in verifica durante la finestra di rotazione. **Stato in v1**: il segmento è caricato in configurazione ma non ancora cablato a runtime — l'`Authenticator` riceve un solo segreto (`KIGUMI_JWT_SECRET`), quindi la verifica con il segreto precedente non è ancora attiva. Vedi anche [sicurezza.md](sicurezza.md).

### Operazioni distruttive

`KIGUMI_ADMIN_TOKEN` è un segreto **riservato** alla futura protezione delle operazioni distruttive sul database (dump/restore/gc): in v1 viene caricato nella configurazione ma l'enforcement non è ancora cablato e quei comandi/endpoint non esistono ancora. Coerente con [configurazione.md](configurazione.md).
