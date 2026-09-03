# Configurazione (riferimento)

Questa pagina documenta **ogni** chiave di configurazione di Kigumi, il suo valore di default e il suo significato. La configurazione di Kigumi è divisa in due piani nettamente separati: la **configurazione di boot** (non segreta, tipizzata, caricata da `kigumi.toml` e dalle variabili d'ambiente con prefisso `KIGUMI_CONF_`) e i **segreti** (letti esclusivamente dall'ambiente, mai dal file). A questi si aggiungono le **runtime settings** memorizzate nel database, che sono mutabili senza riavvio e per le quali il DB è l'unica autorità. La pagina include inoltre il riferimento completo dei comandi della CLI `kigumi`. Per l'installazione vedi [installazione.md](installazione.md); per il contesto architetturale vedi [architettura.md](architettura.md); per i temi di sicurezza (rotazione JWT, gruppi, ACL e record rule) vedi [sicurezza.md](sicurezza.md).

## I due piani: boot-time e segreti

La configurazione di boot è l'insieme di tutto ciò che è serializzabile in `kigumi.toml` e che **non** è un segreto. Viene caricata con la stratificazione `defaults < kigumi.toml < env KIGUMI_CONF_*`, parsificata nella struttura tipizzata `Config` e validata **fail-fast**: un refuso in una sezione core impedisce l'avvio, invece di essere silenziosamente ignorato.

I segreti non stanno mai nel file: sono letti solo dall'ambiente tramite `Secrets::from_env`, e la presenza di quelli obbligatori è verificata all'avvio. L'identità della connessione al database è il singolo `DATABASE_URL` (un DSN completo); la sezione `[database]` porta solo il *tuning* non legato all'URL, così non c'è sovrapposizione ambigua.

`Settings::load` combina i due piani in una struttura `Settings { config, secrets }` e ne verifica anche l'interazione (per esempio: se `[mail].smtp_host` è impostato ma `KIGUMI_SMTP_PASSWORD` non lo è, l'avvio fallisce).

## File `kigumi.toml`

Esempio completo (copia di `kigumi.toml.example`):

```toml
[instance]
name = "acme-prod"

[server]
bind = "0.0.0.0:8099"
workers = 8
proxy_mode = true              # trust X-Forwarded-* behind a reverse proxy
# Browser origins allowed to call the API cross-origin. Omitted/empty = no CORS layer at all
# (same-origin only). ["*"] allows any origin — fine for a public read API, not for one a
# browser session can reach.
# cors_allowed_origins = ["https://app.example.com"]

[database]                     # TUNING ONLY — the connection identity is the DATABASE_URL env var
pool_max = 10
connect_timeout = "5s"

[storage]
backend = "fs"                 # fs | s3
path = "/var/lib/kigumi/blobs"
# bucket = "kigumi-blobs"     # for backend = s3 (keys via env)
# region = "eu-west-1"

[auth]
access_ttl = 900               # 15 min  (jwt secret via env KIGUMI_JWT_SECRET)
refresh_ttl = 2592000          # 30 days

[mail]
smtp_host = "smtp.acme.com"    # smtp password via env KIGUMI_SMTP_PASSWORD
smtp_port = 587
from = "erp@acme.com"

[modules]
load = ["base", "sales"]

[modules.sales]                # OPEN subtree — validated by the "sales" module, not the core schema
default_tax = "0.22"

[log]
level = "info"                 # error | warn | info | debug | trace
format = "json"                # json | text  (default del codice: text)
```

Le sezioni core sono dichiarate con `deny_unknown_fields`: una chiave o una sezione con un refuso fa fallire il caricamento. Fa eccezione `[modules]` (vedi sotto), il cui sottoalbero per-modulo è volutamente aperto.

## Riferimento delle chiavi

Nota sui default: la `Config` di default è completa ma **non** automaticamente valida. In particolare, con `storage.backend = "fs"` è obbligatorio fornire `storage.path` (vedi [Validazione](#validazione)). I default elencati sotto sono quelli applicati quando la chiave è assente.

### `[instance]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `name` | `String` | `"kigumi"` | Nome logico dell'istanza. |

> Nota: i valori runtime dell'istanza (`base_url`, `mode`, `neutralized`, `banner`) **non** stanno qui: vivono nel database e lì sono autoritativi. Vedi [Runtime settings nel database](#runtime-settings-nel-database).

### `[server]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `bind` | `String` | `"127.0.0.1:8099"` | Indirizzo `host:port` su cui il server ascolta. Deve essere parsabile come `SocketAddr`, altrimenti la validazione fallisce. |
| `workers` | `usize` | `4` | Numero di worker. |
| `proxy_mode` | `bool` | `false` | Quando `true`, l'istanza si fida degli header `X-Forwarded-*` dietro un reverse proxy. |
| `cors_allowed_origins` | `[string]` | `[]` | Origini browser autorizzate a chiamare l'API cross-origin. Vuoto non monta **alcun layer CORS**: solo same-origin, che è quello che già danno un reverse proxy (o il proxy Vite in dev). `["*"]` autorizza qualunque origine. Le credenziali sono token Bearer in header, mai cookie, quindi `allow_credentials` resta spento e `*` resta legittimo per un'API pubblica in sola lettura. Una voce che non è un'origine valida fa fallire il boot invece di essere scartata in silenzio. |

### `[database]` (solo tuning)

L'identità della connessione (host, porta, db, utente, password, sslmode) è il singolo `DATABASE_URL`. Questa sezione contiene **solo** tuning non legato all'URL; una chiave come `host` qui è un campo sconosciuto e viene rifiutata (lo verifica il test `host_in_database_section_is_rejected`).

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `pool_max` | `u32` | `10` | Dimensione massima del pool di connessioni. |
| `connect_timeout` | `String` | `"5s"` | Timeout di connessione (stringa di durata). |

### `[storage]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `backend` | enum `fs` \| `s3` | `fs` | Backend del blob store content-addressed (`StorageBackend`). |
| `path` | `Option<String>` | assente | Directory radice per il backend `fs`. **Obbligatoria** quando `backend = fs`. |
| `bucket` | `Option<String>` | assente | Bucket per il backend `s3`. **Obbligatorio** quando `backend = s3`. |
| `region` | `Option<String>` | assente | Region per il backend `s3`. |

Per il backend `fs`, `serve` istanzia un `FsBlobStore` (un'`Arc<dyn BlobStore>`) radicato in `storage.path`, e bytes identici deduplicano in un unico file immutabile.

Per il backend `s3`, `serve` istanzia un `S3BlobStore` su `bucket`/`region` con lo stesso dedup content-addressed (chiavi `ab/cd/<sha256>`). Il binario `kigumi` include la feature `s3` già compilata — nessuna ricompilazione necessaria. Due cose arrivano dall'**ambiente**, mai da questo file:

- **Credenziali** — la catena AWS standard: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` e opzionalmente `AWS_SESSION_TOKEN`; con fallback al profilo condiviso e poi al ruolo IAM.
- **Endpoint** — `KIGUMI_S3_ENDPOINT` seleziona un servizio S3-compatibile diverso da AWS (MinIO, Cloudflare R2, LocalStack). Impostarlo passa automaticamente all'addressing path-style. Lascialo assente per AWS S3 reale. Quando `region` è assente, il default è `us-east-1`.

Esempio — MinIO:

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export KIGUMI_S3_ENDPOINT=http://127.0.0.1:9000
# kigumi.toml: [storage] backend = "s3", bucket = "kigumi-blobs"
kigumi serve
```

### `[auth]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `access_ttl` | `u64` | `900` | Durata in secondi dell'access token (15 minuti). |
| `refresh_ttl` | `u64` | `2592000` | Durata in secondi del refresh token (30 giorni). |

Il segreto di firma HS256 non sta qui: arriva da `KIGUMI_JWT_SECRET` (vedi [Segreti](#segreti-variabili-dambiente)).

### `[mail]`

Tutti i campi sono opzionali.

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `smtp_host` | `Option<String>` | assente | Host SMTP. Se impostato, richiede `KIGUMI_SMTP_PASSWORD` nell'ambiente, altrimenti l'avvio fallisce. |
| `smtp_port` | `Option<u16>` | assente | Porta SMTP (per esempio `587`). |
| `from` | `Option<String>` | assente | Indirizzo mittente di default. |

### `[modules]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `load` | `Vec<String>` | `[]` | **Inerte in v1**: la chiave viene letta ma non seleziona i moduli installati. L'installazione è governata dal registro nel DB tramite `kigumi module install` (vedi [moduli.md](moduli.md)). |
| `[modules.<name>]` | sottoalbero aperto | `{}` | Configurazione per-modulo. |

Le chiavi core di `[modules]` sono strette, ma ogni `[modules.<name>]` è un **sottoalbero aperto**: viene catturato verbatim (campo `per_module`, una `BTreeMap<String, figment::value::Value>` con `#[serde(flatten)]`) e validato dal modulo proprietario al load, così un modulo può portare le proprie impostazioni senza che l'istanza si rifiuti di avviarsi. Nell'esempio, `[modules.sales]` con `default_tax = "0.22"` è validato dal modulo `sales`, non dallo schema core. Vedi [moduli.md](moduli.md) e [moduli-custom.md](moduli-custom.md).

### `[log]`

| Chiave | Tipo | Default | Significato |
|--------|------|---------|-------------|
| `level` | `String` | `"info"` | Livello di log: `error` \| `warn` \| `info` \| `debug` \| `trace`. |
| `format` | `String` | `"text"` | Formato di log: `json` \| `text`. (L'esempio usa `json`.) |

`serve` installa un subscriber `tracing` da questi valori: `format = "json"` produce log strutturati per una pipeline di produzione, `text` è leggibile dall'uomo. La variabile d'ambiente **`RUST_LOG`** sovrascrive `level` quando impostata (es. `RUST_LOG=kigumi_server=debug,info`). Ogni richiesta HTTP è avvolta in uno span che logga metodo, path, status e latenza (richieste completate a `info`, fallimenti a `error`) — **solo metadati; body di richiesta e risposta non vengono mai loggati**. L'export di metriche/tracce verso un collector OpenTelemetry è il layer opt-in successivo.

### `[oidc]`

SSO opzionale via OpenID Connect (Authorization Code + PKCE), accanto al login password. È **all-or-nothing**: o ometti la sezione (SSO off, le route `/auth/oidc/*` danno 404) o imposti tutte e quattro le chiavi (un blocco parziale fallisce la validazione).

| Chiave | Tipo | Significato |
|--------|------|-------------|
| `issuer` | `String` | L'issuer URL dell'IdP — da `<issuer>/.well-known/openid-configuration` si scoprono gli endpoint authorization/token/JWKS. Qualsiasi IdP conforme (Google, Microsoft, Okta, Keycloak, …). |
| `client_id` | `String` | Il client id OAuth registrato presso l'IdP. |
| `redirect_uri` | `String` | L'URL `…/auth/oidc/callback` del server stesso, registrato presso l'IdP. |
| `post_login_url` | `String` | Dove atterra il browser dopo un login riuscito; i token arrivano nel **fragment** dell'URL (`#access_token=…&refresh_token=…`) perché la SPA li legga. |

Il **secret** del client arriva dalla env var `KIGUMI_OIDC_CLIENT_SECRET`, mai da questo file. Al primo login un'email sconosciuta (verificata) viene provisionata just-in-time con **nessun gruppo** (può autenticarsi ma non vede nulla finché un admin non concede i gruppi) e senza password utilizzabile; un'email nota entra nell'utente esistente. Solo le email che l'IdP marca **verified** sono accettate.

## Validazione

`Config::validate` esegue i controlli incrociati che lo schema serde non può esprimere:

- `storage.backend = fs` richiede `storage.path` (altrimenti errore `storage.backend = fs requires storage.path`).
- `storage.backend = s3` richiede `storage.bucket` (altrimenti errore `storage.backend = s3 requires storage.bucket`).
- `server.bind` deve parsare come `host:port` (`SocketAddr`), altrimenti errore `server.bind is not a host:port (...)`.

A livello di `Settings::load` c'è inoltre il controllo incrociato con i segreti: `mail.smtp_host` impostato senza `KIGUMI_SMTP_PASSWORD` produce l'errore `mail.smtp_host is set but KIGUMI_SMTP_PASSWORD is not`.

Per validare e ispezionare la configurazione effettiva senza avviare il server puoi usare il binario standalone `kigumi-config`:

```bash
kigumi-config check    # valida la configurazione effettiva (config + segreti)
kigumi-config print    # stampa la config effettiva con i segreti redatti
```

Il path del file è preso da `$KIGUMI_CONFIG` o, in assenza, da `./kigumi.toml`. I due comandi sono disponibili anche come sottocomandi `kigumi config check` / `kigumi config print` (quest'ultimo, a differenza del binario standalone, aggiunge anche le runtime settings dal DB).

## Override da variabili d'ambiente: `KIGUMI_CONF_`

Ogni chiave di boot è sovrascrivibile dall'ambiente con il prefisso `KIGUMI_CONF_` e il **doppio underscore** (`__`) come separatore di annidamento. Il provider env è caricato come ultimo strato (`Env::prefixed("KIGUMI_CONF_").split("__")`), quindi vince su file e default.

| Chiave TOML | Variabile d'ambiente |
|-------------|----------------------|
| `[server] bind` | `KIGUMI_CONF_SERVER__BIND` |
| `[server] workers` | `KIGUMI_CONF_SERVER__WORKERS` |
| `[server] proxy_mode` | `KIGUMI_CONF_SERVER__PROXY_MODE` |
| `[server] cors_allowed_origins` | `KIGUMI_CONF_SERVER__CORS_ALLOWED_ORIGINS` |
| `[storage] backend` | `KIGUMI_CONF_STORAGE__BACKEND` |
| `[storage] path` | `KIGUMI_CONF_STORAGE__PATH` |
| `[auth] access_ttl` | `KIGUMI_CONF_AUTH__ACCESS_TTL` |
| `[log] level` | `KIGUMI_CONF_LOG__LEVEL` |
| `[instance] name` | `KIGUMI_CONF_INSTANCE__NAME` |

Esempio:

```bash
export KIGUMI_CONF_SERVER__BIND=0.0.0.0:9000
export KIGUMI_CONF_SERVER__WORKERS=8
export KIGUMI_CONF_LOG__FORMAT=json
```

Il prefisso `KIGUMI_CONF_` è deliberatamente distinto da quello dei segreti (`DATABASE_URL`, `KIGUMI_JWT_SECRET`, …), così i segreti non vengono mai catturati dal layer di configurazione.

## Segreti (variabili d'ambiente)

I segreti sono letti solo dall'ambiente (mai da `kigumi.toml`) tramite `Secrets::from_env`. La presenza di quelli **obbligatori** è verificata all'avvio: l'istanza si rifiuta di partire se ne manca uno (fail-fast).

| Variabile | Obbligatoria | Significato |
|-----------|--------------|-------------|
| `DATABASE_URL` | Sì | DSN Postgres completo: l'unica fonte dell'identità di connessione (host, porta, db, utente, password, sslmode). Deve essere un URL con schema `postgres` o `postgresql` parsabile, altrimenti errore `DATABASE_URL is not a valid postgres:// URL`. |
| `KIGUMI_JWT_SECRET` | Sì | Segreto di firma HS256 per access e refresh token. |
| `KIGUMI_JWT_SECRET_OLD` | No | Segreto JWT precedente, **riservato** alla rotazione: caricato in `Secrets` e mostrato redatto da `print`, ma l'`Authenticator` accetta ancora un solo segreto, quindi la verifica col segreto vecchio **non è attiva**. Per questo non compare in `.env.example`. |
| `KIGUMI_SMTP_PASSWORD` | No (*) | Password SMTP. (*) Diventa obbligatoria se `[mail].smtp_host` è configurato. |
| `KIGUMI_ADMIN_TOKEN` | No | Bearer token destinato a proteggere operazioni distruttive sul database (dump/restore/gc). Opzionale al boot; quando presente viene solo caricato in `Secrets` e mostrato redatto da `print` (l'enforcement lato endpoint non è ancora cablato). |
| `KIGUMI_OIDC_CLIENT_SECRET` | No (*) | Secret del client OIDC. (*) Diventa obbligatorio quando la sezione `[oidc]` è configurata. |

Una variabile è considerata "non impostata" sia se assente sia se vuota (`req`/`opt` filtrano le stringhe vuote).

Esempio (vedi `.env.example`):

```bash
# REQUIRED — single source of the database connection identity (full Postgres DSN)
DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
# REQUIRED — HS256 signing secret for access/refresh tokens
KIGUMI_JWT_SECRET=CHANGE_ME_long_random_value
# OPTIONAL — required only if [mail].smtp_host is configured
# KIGUMI_SMTP_PASSWORD=
```

Quando la configurazione effettiva viene stampata (`kigumi config print` o `kigumi-config print`), ogni segreto è redatto a livello di campo: la password del `DATABASE_URL` è mascherata (`redact_db_url`) mentre host/porta/db/utente restano visibili, e gli altri segreti compaiono come `set (****)` o `unset`.

### Segreti operativi addizionali

Oltre ai segreti gestiti da `Secrets`, alcuni comandi della CLI leggono direttamente queste variabili d'ambiente:

| Variabile | Usata da | Significato |
|-----------|----------|-------------|
| `KIGUMI_ADMIN_PASSWORD` | `serve` (bootstrap admin) | Password con cui viene bootstrappato l'utente `admin` se non esiste ancora. Senza di essa, `serve` avvisa (`warning: no admin user; set KIGUMI_ADMIN_PASSWORD to bootstrap one`) e non crea l'admin (nessuna password è mai hardcoded). |
| `KIGUMI_NEW_PASSWORD` | `user create`, `user set-password` | Password alternativa al flag `--password` per creare/reimpostare un utente. |
| `KIGUMI_CONFIG` | tutti i comandi | Path del file `kigumi.toml` se non passato con `--config`; in assenza si usa `./kigumi.toml`. |

## Runtime settings nel database

Alcune impostazioni **non** stanno nella configurazione di boot: vivono nel database, sono mutabili senza riavvio e il DB è la loro autorità unica. Sono memorizzate nella tabella `kigumi_setting` (colonne `key`, `value`, `vtype`), l'equivalente tipizzato di un parametro di configurazione runtime.

Due meccanismi distinti popolano questa tabella:

- `seed_setting(key, value, vtype)` inserisce un default **solo se la chiave è assente** (`ON CONFLICT (key) DO NOTHING`): i default di install-time non sovrascrivono mai una modifica dell'operatore.
- `set_setting(key, value, vtype)` fa upsert (`ON CONFLICT (key) DO UPDATE`) e **sovrascrive** sempre il valore esistente.

In fase di migrazione, `serve`/`migrate` seedano i default runtime senza calpestare eventuali modifiche già fatte dall'operatore:

```rust
db.seed_setting("base_url", "", "string").await?;
db.seed_setting("mode", "production", "string").await?;
```

Altre chiavi runtime tipiche sono `neutralized` e `banner`. Il campo `vtype` (`string` \| `bool` \| `int` \| `json`) è un suggerimento per i lettori tipizzati (default `string`). Queste chiavi si gestiscono dalla CLI con `kigumi config set|get` (vedi sotto).

## Riferimento della CLI `kigumi`

L'eseguibile `kigumi` è il comando unico per operare un'istanza. Tutti i comandi accettano l'opzione globale `--config <path>` (in alternativa a `$KIGUMI_CONFIG`).

| Comando | Cosa fa |
|---------|---------|
| `kigumi serve` | Migra il catalogo + lo schema auth, bootstrappa un admin da `KIGUMI_ADMIN_PASSWORD`, quindi serve l'API protetta. Avvia anche lo scheduler dei cron in background (tick ogni 60 s, claim atomico `SKIP LOCKED`). |
| `kigumi migrate` | Migra i modelli dei moduli installati + gli schemi del framework, poi esce. Su un DB fresco installa `base` (e la sua chiusura di dipendenze); seeda i default runtime `base_url`/`mode`. |
| `kigumi module list` | Elenca i moduli disponibili (linkati) indicando per ciascuno lo stato `installed` o `available` e il suo summary. |
| `kigumi module install <name>` | Installa un modulo e la sua chiusura di dipendenze (deps prima), poi migra le loro tabelle (idempotente). |
| `kigumi module uninstall <name>` | Disinstalla un modulo: smette di essere migrato/servito, ma le sue tabelle e i suoi dati sono **mantenuti**. Rifiuta `base` e rifiuta se un modulo installato dipende ancora da quello. |
| `kigumi user create <login> [--password <p>] [--groups <csv>]` | Crea o sostituisce un utente (upsert). Password via `--password` o `$KIGUMI_NEW_PASSWORD`. `--groups` default `user`. |
| `kigumi user set-password <login> [--password <p>]` | Reimposta la password di un utente mantenendone i gruppi. |
| `kigumi user grant <login> <group>` | Aggiunge un gruppo a un utente. |
| `kigumi user company <login> [--active <id>] [--allowed <csv>]` | Assegna lo scope multi-azienda dell'utente: `--active` è la company di default, `--allowed` un CSV di id accessibili (la company attiva è sempre inclusa). Vuoto = senza restrizioni. |
| `kigumi acl grant <model> <group> [--read] [--write] [--create] [--delete]` | Concede (o aggiorna) un'ACL runtime per un gruppo su un modello. Almeno un flag operazione è obbligatorio. |
| `kigumi acl revoke <model> <group>` | Rimuove un override ACL runtime per un gruppo su un modello (la baseline statica resta invariata). |
| `kigumi acl list` | Elenca le ACL effettive: la baseline compilata + gli override runtime dal DB. |
| `kigumi rule add <model> [--groups <csv>] [--ops <csv>] --domain <json>` | Aggiunge una record rule runtime. `--groups` è un CSV (vuoto = globale), `--ops` un CSV di `r`/`w`/`c`/`d` (default `r`), `--domain` l'AST JSON portabile (es. `{"field":"state","op":"!=","value":"done"}`). |
| `kigumi rule remove <id>` | Rimuove una record rule runtime per id (la baseline statica non è toccata). |
| `kigumi rule list` | Elenca le record rule runtime presenti nel DB. |
| `kigumi config check` | Valida la configurazione effettiva. |
| `kigumi config print` | Stampa la configurazione effettiva (segreti redatti) **più** le runtime settings dal DB. |
| `kigumi config set <key> <value> [--vtype <t>]` | Imposta una runtime setting nel DB (autorità per le chiavi runtime). `--vtype` default `string`. |
| `kigumi config get <key>` | Legge il valore di una runtime setting. |
| `kigumi version` | Stampa la versione del framework e i moduli linkati con la loro versione. |

Le ACL e le record rule gestite dalla CLI sono **additive** sopra la baseline compilata: per le ACL gli override del DB possono solo ampliare l'accesso (unione), per le record rule aggiungono restrizioni/alternative attraverso lo stesso motore — in entrambi i casi la baseline statica resta in vigore. Per i dettagli su gruppi, ACL e record rule vedi [sicurezza.md](sicurezza.md).

> Solo i moduli il cui crate è linkato nel binario sono disponibili a `module install`/`list`. Per come dichiarare e impacchettare un modulo vedi [moduli.md](moduli.md) e [moduli-custom.md](moduli-custom.md).
