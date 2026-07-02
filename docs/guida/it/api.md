# API REST e contratto-UI

Kigumi è un framework ERP headless: l'unica superficie di integrazione è un'API HTTP generata dal catalogo dei modelli installati. Questa pagina documenta l'intera API esposta dal crate `kigumi-server` (router axum in `crates/kigumi-server/src/lib.rs`): il flusso di autenticazione JWT, le route dati CRUD con il loro envelope di risposta, gli endpoint dei metodi di servizio (variant, pricelist, wizard, sconto, report, posting, fatturazione, validazione trasferimenti), gli endpoint di allegati e chatter (messaggi, attività, follower), la forma del contratto-UI emesso per modello, il documento OpenAPI, il formato degli errori con i relativi status code e gli endpoint di health. Per il contesto su come questi pezzi si compongono vedi [architettura.md](architettura.md); per l'avvio del server vedi [installazione.md](installazione.md) e [README.md](README.md); per ACL, record rule e scope multi-azienda vedi [sicurezza.md](sicurezza.md).

## Come è montato il router

Il server espone due livelli a seconda di come l'host (la CLI `kigumi serve`) lo costruisce:

- **Router solo-metadati** — `router(models)`: monta unicamente `GET /openapi.json`, `GET /api/models` e `GET /api/:name/view`. Nessun database, nessuna autenticazione.
- **Router completo** — `router_with_data(...)` (o `router_with_data_rasterized(...)` per attaccare un rasterizzatore PDF): aggiunge il blocco `/auth/*`, gli endpoint di health, e tutte le route dati CRUD + metodi di servizio + allegati + chatter, con un `DataBackend` che porta database, ACL, record rule, `Authenticator` e blob store.

La base condivisa da entrambi è:

```rust
fn base_router() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_handler))
        .route("/api/models", get(models_handler))
        .route("/api/:name/view", get(view_handler))
}
```

Il segmento `:name` è il **nome puntato del modello** (es. `sale.order`, `res.partner`, `product.template`), non il nome di tabella. La CLI passa il segreto di firma come `s.secrets.jwt_secret`, cioè la env var **`KIGUMI_JWT_SECRET`** (vedi [configurazione.md](configurazione.md)). Il limite massimo del corpo richiesta è 25 MiB (`DefaultBodyLimit::max(MAX_BODY_BYTES)`, con `MAX_BODY_BYTES = 25 * 1024 * 1024`).

## Autenticazione

L'autenticazione è basata su token JWT HS256 firmati con `KIGUMI_JWT_SECRET`. La logica di emissione e verifica vive nel crate `kigumi-auth` (`Authenticator`). Esistono due **tipi** di token, distinti dal claim `kind`:

- **access token** — breve durata (`ACCESS_TTL = 900` secondi, 15 minuti). Si verifica in un `Ctx` (uid, gruppi, scope multi-azienda) per ogni richiesta dati. Porta dentro di sé i gruppi e lo scope, così ogni richiesta è verificabile senza round-trip al database.
- **refresh token** — lunga durata (`REFRESH_TTL = 2_592_000` secondi, 30 giorni), tracciato server-side da un `jti` e revocabile/ruotabile.

I due tipi sono separati a livello crittografico: un refresh token **non** può mai essere usato come bearer per accedere ai dati, e viceversa (il claim `kind` viene controllato in `decode_kind`). L'algoritmo è fissato a HS256 (`Validation::new(Algorithm::HS256)`, che rifiuta `alg=none` e la confusione di algoritmo) e l'expiry è validato senza finestra di tolleranza (`validation.leeway = 0`).

### `POST /auth/login`

Corpo: `{ "login": "...", "password": "..." }`. Le credenziali mancanti danno `400 login and password required`. La password è verificata con argon2 contro l'hash memorizzato; il login esegue **sempre** argon2 (contro un hash fittizio se l'utente non esiste, tramite `dummy_hash`), così tempi e corpo del `401` sono identici per utente sconosciuto e password errata (nessuna user enumeration). In caso di successo risponde `200` con la coppia di token:

```json
{
  "access_token": "<jwt>",
  "refresh_token": "<jwt>",
  "token_type": "Bearer",
  "expires_in": 900
}
```

Credenziali non valide → `401 invalid credentials`.

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$KIGUMI_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')
```

> L'utente `admin` viene creato al primo `kigumi serve` a partire da `KIGUMI_ADMIN_PASSWORD` (se nessun admin esiste ancora); vedi [installazione.md](installazione.md).

### Header `Authorization: Bearer`

Ogni route dati richiede l'header `Authorization: Bearer <access_token>`. La verifica avviene nel wrapper `authenticate` del server, che delega all'`Authenticator::verify_bearer` di `kigumi-auth`: l'header deve iniziare con il prefisso letterale `Bearer ` e il token deve essere un access token valido. Il fallimento produce `401 unauthorized`. Il `Ctx` derivato è l'unica identità di cui si fida il server: un client non può dichiarare un gruppo senza un token firmato dal segreto.

```bash
curl -s http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN"
```

### `POST /auth/refresh`

Corpo: `{ "refresh_token": "..." }` (mancante → `400 refresh_token required`). Il token presentato viene verificato; poi il server lo **rivendica atomicamente** (`claim_refresh`), revocandolo: un replay concorrente rivendica zero righe ed è respinto (`401 invalid refresh token`), quindi niente double-spend. Sul refresh i gruppi (`user_groups`) e lo scope di company (`user_scope`) vengono **riletti** dal database, così riassegnazioni di gruppo o di company hanno effetto. La risposta è una **nuova** coppia `access_token`/`refresh_token` (rotazione del refresh token) con la stessa forma del login.

### `POST /auth/logout`

Corpo: `{ "refresh_token": "..." }`. Risponde **sempre** `204 No Content`, senza rivelare se il token fosse valido. Se il token è verificabile, il suo `jti` viene revocato server-side (`revoke_refresh`).

### `GET /auth/me`

Restituisce l'identità del chiamante autenticato, cioè il `Ctx` derivato dal bearer. Richiede un access token valido.

```json
{
  "uid": 1,
  "groups": ["user", "admin", "sales.user"],
  "company_id": 1,
  "allowed_company_ids": [1, 2]
}
```

## Endpoint dati (CRUD)

Tutte le route sotto `/api/:name` richiedono un access token e applicano il motore ACL + record rule + scope multi-azienda nello strato `kigumi-db` (i metodi `*_secured`). L'autorizzazione **non** è nel server: l'handler autentica, modella la risposta e mappa gli errori.

| Route | Metodo | Cosa fa |
|---|---|---|
| `/api/models` | GET | array JSON dei nomi dei modelli serviti |
| `/api/:name/view` | GET | contratto-UI del modello (vedi sotto) |
| `/api/:name` | GET | lista paginata (envelope `data/total/limit/offset`) |
| `/api/:name` | POST | crea un record, ritorna `{ "id": <n> }` con `201` |
| `/api/:name/:id` | GET | legge un record |
| `/api/:name/:id` | PATCH | aggiorna un record, ritorna `{ "updated": <n> }` |
| `/api/:name/:id` | DELETE | elimina un record, ritorna `{ "deleted": <n> }` |
| `/api/:name/:id/action/:action` | POST | esegue un'azione di transizione di stato |

### `GET /api/models`

Array JSON dei nomi (puntati) dei modelli serviti, es. `["res.partner", "sale.order", ...]`.

### `GET /api/:name` — lista paginata

Risponde con un envelope a quattro campi:

```json
{ "data": [ /* record */ ], "total": 123, "limit": 80, "offset": 0 }
```

- `data` — la pagina di record (la `ListPage.data` dello strato db).
- `total` — il conteggio totale sotto lo **stesso** filtro sicuro (non solo la pagina).
- `limit` / `offset` — i valori effettivamente applicati (riecheggiati).

#### Parametri di paginazione, ordinamento e filtro

| Parametro | Significato | Note |
|---|---|---|
| `limit` | dimensione pagina | default `80` (`DEFAULT_LIMIT`); clampato in `[1, 500]` (`MAX_LIMIT`); non intero → `400 limit must be an integer` |
| `offset` | scostamento | default `0`; valori negativi portati a `0`; non intero → `400 offset must be an integer` |
| `order` | ordinamento | lista separata da virgola; prefisso `-` = discendente, es. `-id` o `name,-amount_total` |
| `domain` | AST di dominio JSON | escape per AND/OR/NOT arbitrari (vedi sotto) |
| `<field>__<op>=<value>` | filtro a operatore-suffisso | il filtro di default, condizioni AND-ate |

Esistono **due forme di filtro** (decisione D5), combinabili (AND-ate quando entrambe presenti):

1. **Operatore-suffisso** `field__op=value` (gestito da `split_suffix` + `build_leaf`). Un `field` nudo senza suffisso usa l'operatore `eq`. Operatori riconosciuti:

   | Suffisso | Operatore |
   |---|---|
   | `eq` | `=` |
   | `ne` | `!=` |
   | `gt` | `>` |
   | `gte` | `>=` |
   | `lt` | `<` |
   | `lte` | `<=` |
   | `like` | `LIKE` |
   | `ilike` | `ILIKE` |
   | `in` | `IN` (valore = lista separata da virgola) |

   Il valore è coercito al tipo del campo (`coerce`): un suffisso ignoto, un campo ignoto, o un valore non coercibile (es. `'nope'` su un campo intero) danno `400`. Non si può filtrare direttamente su un campo `One2many`/`Many2many`. Il campo `id` è sempre filtrabile (trattato come intero).

2. **AST di dominio** `?domain=<json>` (parsato da `Domain::from_json`). JSON-encoded; respinto con `400 invalid domain JSON` se malformato. La forma è la stessa che il server compila in SQL e che il frontend valuta client-side (vedi [Contratto-UI](#contratto-ui)). Nodi:

   ```json
   { "field": "state", "op": "=", "value": "draft" }
   { "and": [ {"field":"state","op":"=","value":"draft"}, {"field":"amount","op":">=","value":100} ] }
   { "or":  [ /* ... */ ] }
   { "not": { /* ... */ } }
   { "const": true }
   ```

   I token di `op` ammessi nell'AST (`op_from_str`) sono: `=`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not in`, `like`, `ilike`, `is null`, `is not null` (per `is null`/`is not null` il `value` è omesso).

Esempi:

```bash
# operatore-suffisso + ordinamento + paginazione
curl -s "http://127.0.0.1:8099/api/sale.order?state=draft&amount_total__gte=100&order=-id&limit=20" \
  -H "Authorization: Bearer $TOKEN"

# AST di dominio
curl -s -G "http://127.0.0.1:8099/api/sale.order" \
  --data-urlencode 'domain={"or":[{"field":"state","op":"=","value":"draft"},{"field":"state","op":"=","value":"sent"}]}' \
  -H "Authorization: Bearer $TOKEN"
```

### `POST /api/:name` — crea

Corpo: un oggetto JSON (un corpo non-oggetto → `400 body must be a JSON object`). In caso di successo `201 Created` con `{ "id": <nuovo id> }`.

```bash
curl -s -X POST http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"ACME Spa"}'
# 201 → {"id": 42}
```

### `GET /api/:name/:id` — leggi

Restituisce il record come oggetto JSON (`find_one_secured`). I figli `One2many` sono **inlined** come oggetti figlio completi nel get-one. Se il record non esiste o non è permesso dal motore di sicurezza → `404 not found or not permitted` (la non-esistenza e la non-visibilità sono indistinguibili, per non rivelare l'esistenza di record non accessibili).

### `PATCH /api/:name/:id` — aggiorna

Corpo: oggetto JSON con i campi da scrivere. `0` righe aggiornate (record assente o non permesso) → `404 not found or not permitted`; altrimenti `200` con `{ "updated": <n> }`.

### `DELETE /api/:name/:id` — elimina

`0` righe → `404 not found or not permitted`; altrimenti `200` con `{ "deleted": <n> }`.

### `POST /api/:name/:id/action/:action` — azione di transizione di stato

Esegue un'azione registrata (`run_action`, es. confermare un ordine in bozza). In caso di successo:

```json
{ "ok": true, "action": "confirm" }
```

Gli errori (azione sconosciuta, accesso negato, transizione non valida) seguono `write_error` (vedi [Formato degli errori](#formato-degli-errori-e-status-code)).

## Endpoint dei metodi di servizio

I metodi di business cross-record registrati dai moduli sul giunto `register_service!` passano dal dispatch generico:

`POST /api/:name/:id/service/:service` — body: un oggetto JSON (l'input del servizio), risultato: l'output JSON del servizio. Il servizio possiede UNA transazione (commit sul successo, rollback sull'errore, inclusi i job accodati al suo interno); il write gate opzionale richiede che il chiamante abbia Write sul modello, più l'eventuale restrizione di gruppo dichiarata dalla registrazione.

Alcuni metodi legacy precedono il giunto e mantengono route dedicate con pin. L'handler verifica il **pin** del modello (un nome diverso → `400`), autentica e modella la risposta; l'autorizzazione e la logica transazionale vivono nello strato `kigumi-db`. Se il modello non è servito (modulo non installato) → `404`.

| Route | Metodo | Modello richiesto | Risultato JSON | Status |
|---|---|---|---|---|
| `/api/:name/:id/generate_variants` | POST | `product.template` | `{ "created": [...], "archived": [...], "kept": [...] }` | `200` |
| `/api/:name/:id/apply_pricelist` | POST | `sale.order` | `{ "priced": <n> }` | `200` |
| `/api/:name/open` | POST | qualsiasi wizard | il record transiente creato | `201` |
| `/api/:name/:id/apply_discount` | POST | `sale.order.discount` | `{ "discounted": <n> }` | `200` |
| `/api/:name/:id/report/:report` | GET | qualsiasi (con report registrato) | HTML, o PDF con `?format=pdf` | `200` |
| `/api/:name/:id/post` | POST | `account.move` | `{ "posted": "<numero>" }` | `200` |
| `/api/:name/:id/create_invoice` | POST | `sale.order` | `{ "invoice": <move_id> }` | `200` |
| `/api/:name/:id/validate` | POST | `stock.picking` | `{ "validated": "<numero>" }` | `200` |
| `/api/:name/:id/create_delivery` | POST | `sale.order` | `{ "picking": <id> }` | `201` |
| `/api/:name/:id/create_receipt` | POST | `purchase.order` | `{ "picking": <id> }` | `201` |

Note di dettaglio:

- **`generate_variants`** — materializza il prodotto cartesiano delle attribute line di un `product.template` in `product.product`; il risultato distingue gli id creati, archiviati (una combinazione non più selezionata) e mantenuti.
- **`apply_pricelist`** — ri-prezza le righe di un `sale.order` dalla sua pricelist; `priced` è il numero di righe ri-prezzate.
- **`open` (wizard)** — apre un modello transiente: calcola i suoi default server-side (`default_get`) dal contesto di apertura (`active_model` / `active_id` / `active_ids`, tutti opzionali nel corpo), crea la riga scratchpad sotto il chiamante e la restituisce per il render via contratto. Il modello deve essere bound con `register_wizard!` (altrimenti `400 not a wizard model`).
- **`report`** — la sicurezza è l'accesso in lettura al record (`find_one_secured`): poter leggere il record è esattamente ciò che permette di stamparlo. Un nome di report sconosciuto → `404 unknown report`. Senza `?format=pdf` risponde HTML (`text/html`); con `?format=pdf` rasterizza lo stesso HTML in PDF — ma solo se è configurato un rasterizzatore, altrimenti `501 PDF rendering is not configured`.
- **`post`** — posta una `account.move` in bozza (ricontrollo del bilancio + numerazione per giornale + stato → `posted`); ritorna il numero assegnato.
- **`validate`** — valida un `stock.picking` in bozza (movimenti `done` + aggiornamento giacenze + numerazione, in una transazione).
- **`create_delivery` / `create_receipt`** — creano un trasferimento in bozza (`Stock → Customers` da un `sale.order` confermato; `Vendors → Stock` da un `purchase.order` confermato) e ritornano `201` con l'id del trasferimento.

Dal client web questi metodi record-scoped passano per un singolo helper:

```ts
// web/src/api.ts
export async function callEndpoint<T = Record<string, unknown>>(
  model: string,
  id: number,
  path: string,
): Promise<T> {
  return asJson<T>(await request(`/api/${model}/${id}/${path}`, { method: 'POST' }))
}
```

## Route dei moduli: `GET|POST /api/x/:route`

Gli endpoint su misura registrati con `register_route!` (receiver di webhook, ricerche custom) sono smistati genericamente su `/api/x/<name>`, con chiave `(name, method)`. Le route autenticate girano sotto il `Ctx` del chiamante (più l'eventuale restrizione di gruppo); le route `auth: false` girano sotto il contesto GUEST (uid −1, zero gruppi — la ACL default-deny blocca ogni chiamata secured finché il body non verifica da sé il mittente, ad esempio con la `RouteInput::verify_hmac_sha256` constant-time). I body delle richieste sono limitati a 2 MB; un metodo sbagliato su un nome esistente risponde `405` con header `Allow`.

## Eventi live (SSE): `GET /api/events/stream`

Server-sent events per ogni scrittura committata, filtrati per chiamante: un evento è consegnato solo se il chiamante può leggere il record adesso (ACL + record rule riverificate a ogni batch), i nomi dei campi modificati sono filtrati per visibilità dei field group, e gli eventi di cancellazione sono soppressi dove si applica una record rule di lettura. Ogni evento porta un id nella forma `txn:id`; riconnettiti con `Last-Event-ID` per una ripresa esatta senza buchi (il cursore è la coppia, quindi nessun evento committato viene saltato o duplicato). Gli stream sono limitati a 15 minuti — i client si riconnettono e la ripresa è trasparente; la revoca degli accessi non resta quindi mai stantia oltre un batch.

```
event: message
id: 668129:15
data: {"type":"model.created","model":"workshop.vehicle","record_id":2,"txn":668129,"changes":{},...}
```

L'autenticazione è lo stesso bearer token (`EventSource` non può inviare header — usa `fetch` con uno stream leggibile, come fa `web/src/api.ts`).

## Report ledger: `GET /api/reports/:name`

Query aggregate senza record (un bilancio di verifica, una valorizzazione di magazzino) registrate con `register_ledger_report!`, che restituiscono righe JSON; ogni report è protetto dalla ACL di lettura del modello che dichiara. Distinti dai report documentali per-record (`/api/:name/:id/report/:report`).

## Allegati

Gli allegati sono righe `ir.attachment`: i metadati stanno nel record, i byte in un blob store content-addressed (deduplicato per checksum SHA-256). Le route ancorate al record host sono gated dall'accesso al record host: list/download richiedono **read** sul host, upload/delete richiedono **write** sul host.

| Route | Metodo | Gate | Risultato |
|---|---|---|---|
| `/api/:name/:id/attachments` | GET | read host | `{ "data": [ /* metadati, niente byte */ ] }` |
| `/api/:name/:id/attachments` | POST | write host | `201` + `{ "id", "name", "mimetype", "file_size", "checksum" }` |
| `/api/attachment/:aid/content` | GET | read del record host a cui è allegato | i byte (stream) |
| `/api/attachment/:aid` | DELETE | write del record host | `{ "deleted": 1 }` |

L'upload manda i **byte grezzi** nel corpo; il nome file viaggia nell'header `X-Filename` e il mimetype nel `Content-Type`. Un upload vuoto → `400 empty upload`. In download, solo un allowlist sicuro (`image/png`, `image/jpeg`, `image/gif`, `image/webp`, `application/pdf`) è servito `inline`; tutto il resto è forzato a `attachment` con `X-Content-Type-Options: nosniff`, così un blob caricato dall'utente non può mai eseguire come script nell'origin dell'app.

```ts
// web/src/api.ts
export async function uploadAttachment(model: string, id: number, file: File): Promise<number> {
  const res = await request(`/api/${model}/${id}/attachments`, {
    method: 'POST',
    headers: { 'content-type': file.type || 'application/octet-stream', 'x-filename': file.name },
    body: file,
  })
  return (await asJson<{ id: number }>(res)).id
}
```

## Chatter: messaggi, attività, follower

Il sottosistema mail aggiunge a un modello che vi aderisce (`mailed = true` nel contratto) un thread di messaggi, attività (to-do) e follower. Tutti questi endpoint sono gated da **read** sul record host: non si può vedere o scrivere nel thread di un record che non si può leggere. Il modello host deve aver aderito a mail (altrimenti `400 model '<name>' has no mail thread`).

| Route | Metodo | Cosa fa |
|---|---|---|
| `/api/:name/:id/messages` | GET | thread del record, dal più vecchio; ogni messaggio porta i suoi diff di tracking |
| `/api/:name/:id/message` | POST | posta un commento o una nota |
| `/api/:name/:id/activities` | GET | to-do aperte, ciascuna con uno `state` derivato |
| `/api/:name/:id/activity` | POST | pianifica una to-do |
| `/api/:name/:id/activities/:aid/done` | POST | segna una to-do come fatta |
| `/api/:name/:id/followers` | GET | utenti iscritti al thread |
| `/api/:name/:id/follow` | POST | iscrive un utente (idempotente) |
| `/api/:name/:id/unfollow` | POST | disiscrive un utente (idempotente) |

Dettagli:

- **Messaggi** — `GET .../messages` risponde `{ "data": [...] }`; ogni messaggio è arricchito con un array `tracking` dei suoi cambi di campo (`old_value`/`new_value`). I cambi su campi che il chiamante non può leggere sono **redatti** (sicurezza a livello di campo, D6, via `field_accessible`), così l'audit trail non diventa un secondo canale di lettura non protetto.
- **Postare** — `POST .../message` richiede un `body` non vuoto (altrimenti `400 message body is required`). `message_type` ammette `comment` (default) o `note`; un altro valore → `400 invalid message_type '<other>'`. L'autore è il chiamante autenticato (`ctx.uid`), il timestamp è il clock del DB.
- **Attività** — lo `state` (`overdue` / `today` / `planned`) è **derivato** (`activity_state`) dalla deadline confrontata con la data corrente del DB (le stringhe ISO si confrontano lessicograficamente). `POST .../activity` richiede un `summary` non vuoto; `date_deadline` è opzionale (vuoto = nessuna deadline); `user_id` opzionale defaulta al chiamante.
- **Done** — `POST .../activities/:aid/done` mette `active` a false; l'attività deve appartenere a quel record host, altrimenti `404 activity not found on this record`.
- **Follow/unfollow** — entrambi idempotenti: ri-seguire un record già seguito è un successo (`{ "ok": true, "already": true }`), disiscriversi quando non si è follower è un successo. Solo il gruppo `admin` può (dis)iscrivere un `user_id` diverso dal proprio (anti-IDOR, via `ensure_self_or_admin`), altrimenti `403 cannot manage another user's subscription`.

## Contratto-UI

`GET /api/:name/view` restituisce il **contratto-UI** del modello: un JSON agnostico consumabile da qualunque frontend, prodotto da `to_ui_contract` in `crates/kigumi-schema/src/lib.rs`. È la stessa fonte di verità del DDL e dell'OpenAPI, proiettata per il rendering del form e della tabella. Un nome di modello sconosciuto → `404 unknown model: <name>`.

Forma generale:

```json
{
  "model": "sale.order",
  "type": "form",
  "mailed": true,
  "fields": [ /* FieldMeta */ ],
  "list": { "columns": [ /* ColumnMeta */ ] },
  "actions": [ /* ActionMeta */ ],
  "reports": [ /* ReportMeta */ ],
  "view": { "groups": [ /* ... */ ], "pages": [ /* ... */ ] }
}
```

### Campi (`fields`)

Ogni campo porta `name`, `label`, un `widget` suggerito dal tipo, `required` e `readonly`. I campi computed e i campi `related` sono `readonly: true` (sono specchi risolti server-side); i campi propri e i campi delegati (`_inherits`) sono editabili. Il `widget` è mappato dal tipo del campo:

| Tipo campo | `widget` |
|---|---|
| Text | `char` |
| Html | `html` |
| Image | `image` |
| Integer | `integer` |
| Float | `float` |
| Decimal con currency | `monetary` |
| Decimal senza currency | `float` |
| Bool | `boolean` |
| Date | `date` |
| Datetime | `datetime` |
| Selection | `selection` |
| Many2one | `many2one` |
| One2many | `one2many` |
| Many2many | `many2many` |

Attributi aggiuntivi opzionali per campo:

- `options` — per i `selection`, l'array `{ "value", "label" }` delle opzioni.
- `relation` — per `many2one`/`one2many`, il modello target; `inverse` per `one2many` (il campo FK inverso).
- `default` — il valore di default dichiarato.
- `invisible_when` / `readonly_when` — un **AST di dominio** (vedi sotto) che, quando vale per il record, rende il campo invisibile/readonly.

Le regole `invisible_when` / `readonly_when` sono emesse come AST di dominio portabili, identici a quelli accettati da `?domain=` e compilati in SQL dal server. Il frontend le valuta client-side **dai dati del record**, mai con una stringa eval'd. Esempio di campo emesso:

```json
{ "name": "confirm_date", "label": "Confirm Date", "widget": "date", "required": false,
  "readonly": false,
  "invisible_when": { "field": "state", "op": "=", "value": "draft" } }
```

Le regole sono **validate** alla costruzione del contratto: una regola che referenzia un campo sconosciuto o di tipo errato fa fallire `to_ui_contract` (le regole UI rotte sono rifiutate, non scoperte in produzione).

### Tabella (`list.columns`)

L'array delle colonne che una tabella generica renderizza (D7): i campi scalari (con colonna) più i computed on-read, gli specchi related e i campi delegati, in ordine di dichiarazione. Un `One2many` non è una colonna. Ogni colonna è `{ "name", "label", "widget" }`.

### Azioni (`actions`)

Le azioni di transizione di stato che un form può offrire come bottoni, ciascuna con i gruppi autorizzati a eseguirla (`groups` vuoto = tutti). Forma: `{ "name", "groups": [...] }`. Il frontend nasconde quelle che i gruppi del chiamante non concedono:

```ts
// web/src/api.ts
export function canRun(action: ActionMeta, identity: Identity | null): boolean {
  if (action.groups.length === 0) return true
  if (!identity) return false
  return action.groups.some((g) => identity.groups.includes(g))
}
```

### Report (`reports`)

I documenti stampabili per un record, ognuno `{ "name", "title" }`. Il `name` è il segmento URL (`GET /api/:name/:id/report/<name>`), il `title` è l'etichetta umana usata anche per il filename del download PDF.

### Vista (`view`)

Il layout del form dichiarato dal modello (`view_for`), oppure `null` quando il modello non dichiara una vista (il frontend applica allora un layout di default intelligente). Quando presente:

```json
{
  "groups": [
    { "title": "Identità", "fields": [ { "name": "name", "full": true }, { "name": "ref", "full": false } ] }
  ],
  "pages": [
    { "title": "Righe", "fields": ["line_ids"] }
  ]
}
```

- `groups` — gruppi titolati di campi scalari (layout a due colonne nel "sheet"); `title` può essere `null` (un gruppo di testa senza intestazione); `full: true` fa estendere il campo su entrambe le colonne (relazioni, testo lungo, immagini, nome primario).
- `pages` — le pagine del notebook (tab) sotto lo sheet, di solito una relazione `One2many` o dettagli secondari; ogni pagina è `{ "title", "fields": [...] }`.

## Documento OpenAPI

`GET /openapi.json` restituisce un documento **OpenAPI 3.1.0** generato dal catalogo dei modelli (`openapi` in `crates/kigumi-schema/src/openapi.rs`). È pretty-printed, con `info.title = "Kigumi API"` e `info.version = "0.1.0"`. È pensato per generare SDK tipizzati (TS/Python/Go) con tooling standard (openapi-generator), senza client scritti a mano.

Per ogni modello emette:

- in `components.schemas`, uno schema oggetto chiave-nome-modello (es. `sale.order`) con `id` (`int64`, `readOnly`) e una proprietà per campo. I decimali sono `string` con `format: decimal` (per preservare la precisione NUMERIC), le date sono `format: date`/`date-time`, i `One2many` sono array di oggetti figlio (`$ref` al modello child), i `Many2many` array di id `int64`, i campi computed sono `readOnly`.
- in `paths`, `GET /api/<table>` (lista) e `GET /api/<table>/{id}` (get-one), con `operationId` `list_<table>` e `get_<table>`.

> **Attenzione — divergenza tra spec e route reali:** l'OpenAPI usa il **nome di tabella** sottolineato (`m.table`, es. `/api/sale_order`), mentre le route dati realmente montate dal server usano il **nome puntato** del modello (`m.name`, es. `/api/sale.order`). Inoltre l'OpenAPI 3.1 generato in questa versione documenta solo `GET` list e `GET` get-one; non descrive ancora gli endpoint di create/update/delete, auth, metodi di servizio, allegati o chatter. Per la lista completa fai riferimento a questa pagina, non solo allo spec.

```bash
curl -s http://127.0.0.1:8099/openapi.json | head -40
```

## Formato degli errori e status code

Gli errori sono restituiti come una busta JSON strutturata, con uno status code che ne indica la classe:

```json
{ "error": { "code": "invalid", "message": "hours cannot be negative", "fields": { "hours": ["hours cannot be negative"] } } }
```

`code` è una classe stabile in kebab-case (`bad-input`, `invalid`, `access-denied`, `conflict`, `internal`); `message` è leggibile; `fields` (presente sugli errori di validazione) mappa i nomi dei campi ai messaggi, pronto per il rendering inline nei form — le violazioni `@api.constrains` portano i campi dichiarati dalla regola, i rifiuti not-null portano la colonna mancante. Il dettaglio interno (schema, SQL, testo errore Postgres) **non** raggiunge mai il client: gli errori non mappati sono loggati server-side e restituiti come busta `500` opaca.


| Status | Quando | Esempio di corpo |
|---|---|---|
| `400 Bad Request` | input non valido: body non-oggetto, campo/operatore/dominio di filtro non valido, valore non coercibile, modello sbagliato per un metodo pinnato, `message_type` non valido, body messaggio/summary mancante, `user_id`/`limit`/`offset` non interi | `body must be a JSON object`, `invalid domain JSON: ...` |
| `401 Unauthorized` | nessun token o token non valido/scaduto; credenziali di login errate; refresh token non valido/già speso | `unauthorized`, `invalid credentials`, `invalid refresh token` |
| `403 Forbidden` | accesso negato dall'ACL / record rule; tentativo di gestire l'iscrizione di un altro utente senza essere admin | `access denied`, `cannot manage another user's subscription` |
| `404 Not Found` | modello sconosciuto; record assente o non permesso; report/allegato/attività non trovati | `unknown model: <name>`, `not found or not permitted`, `unknown report` |
| `409 Conflict` | violazione di vincolo (es. unique) su una scrittura | testo del conflitto |

Status aggiuntivi non-errore o di servizio: `201 Created` (create, upload, open wizard, create_delivery/receipt, follow), `204 No Content` (logout), `501 Not Implemented` (report PDF senza rasterizzatore configurato), `503 Service Unavailable` (readiness, vedi sotto).

Il client web modella questa convenzione con un'`ApiError` che porta lo `status` e il corpo testuale, e ritenta **una sola volta** in modo trasparente su un `401` rinfrescando il token:

```ts
// web/src/api.ts
async function request(path: string, init?: RequestInit, allowRetry = true): Promise<Response> {
  const tokens = loadTokens()
  const headers = new Headers(init?.headers)
  if (tokens) headers.set('authorization', `Bearer ${tokens.access}`)
  const res = await fetch(path, { ...init, headers })
  if (res.status === 401 && allowRetry && tokens && (await tryRefresh())) {
    return request(path, init, false)
  }
  return res
}
```

## Endpoint di health

Due endpoint per le probe del container, montati solo dal router completo:

| Route | Metodo | Cosa fa | Risposta |
|---|---|---|---|
| `/health` | GET | **liveness**: il processo è su. Nessun accesso al DB (probe veloce). | `200` `{"status":"ok"}` |
| `/ready` | GET | **readiness**: il processo può servire traffico (database raggiungibile via `db.ping()`). | `200` `{"status":"ready"}` oppure `503` `{"status":"not_ready"}` |

```bash
curl -s http://127.0.0.1:8099/health   # {"status":"ok"}
curl -s http://127.0.0.1:8099/ready    # {"status":"ready"} oppure 503
```

## Riferimenti

- Router e handler, envelope, status code, `write_error`: `crates/kigumi-server/src/lib.rs`
- Contratto-UI e OpenAPI: `crates/kigumi-schema/src/lib.rs`, `crates/kigumi-schema/src/openapi.rs`
- Emissione/verifica dei token: `crates/kigumi-auth/src/lib.rs`
- AST di dominio (filtri, `invisible_when`/`readonly_when`): `crates/kigumi-core/src/domain.rs`
- Vista del form (gruppi e pagine): `crates/kigumi-core/src/view.rs`
- Client TypeScript della stessa API: `web/src/api.ts`

Vedi anche [moduli.md](moduli.md) e [moduli-custom.md](moduli-custom.md) per come i modelli, le azioni, i report, le viste e i wizard vengono dichiarati e registrati a compile time.
