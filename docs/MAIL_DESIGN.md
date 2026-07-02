# Mini-design: subsystem `mail` per Kigumi (headless-first, più snello di Odoo)

> Analisi grounded sul sorgente Odoo 19 (`mail_thread.py` 5118L, `mail_message.py`, `mail_activity.py`,
> `mail_followers.py`, `mail_tracking_value.py`). Obiettivo: **tenere i punti di forza, eliminare le
> debolezze**, sfruttando il metamodello tipizzato + i registry compile-time + l'unico path di scrittura/
> delete controllato di Kigumi.

## 1. Come fa Odoo (sintesi)
Un modello "diventa" chatter aggiungendo `mail.thread` (+ `mail.activity.mixin`) al suo `_inherit`. Quattro
tabelle condivise polimorfiche servono OGNI modello via `(res_model: Char, res_id: Integer)`:
`mail.message` (log/commenti + audit), `mail.followers` (sottoscrizioni, con M2M ai `mail.message.subtype`),
`mail.activity` (to-do con scadenza), `mail.notification` (consegna per-destinatario). Il tracking dei campi
produce righe `mail.tracking.value` (colonne tipizzate sparse) postate come messaggio. In più: gateway email
in ingresso (`mail.alias`), realtime bus/`Store()` per il client Discuss, e cron per coda mail + GC.

## 2. Punti di forza da TENERE
- **Opt-in riusabile**: una riga e un modello ha chatter/follower/activity/tracking, uniforme ovunque.
- **Tabelle condivise**: una `mail.message` per tutti i modelli — niente proliferazione di tabelle per-modello.
- **Tracking separato dai messaggi** ma renderizzato nello stesso thread (commenti umani + audit di sistema uniformi).
- **`activity.state` DERIVATO dalla scadenza** (overdue/today/planned) → non va mai fuori sync, niente cron di "invecchiamento".
- **Opt-in per-evento granulare** dei follower (un follower può seguire i commenti ma non i cambi di stato).
- **Consegna/lettura per-destinatario** (unread, bounce) tracciabile.

## 3. Debolezze da EVITARE
- **NESSUNA integrità referenziale** sul link polimorfico (`res_model` Char + `res_id` Integer, no FK): orfani
  alla cancellazione, GC manuale via override `unlink()`, lookup lenti senza i due indici compositi a mano.
- **God-mixin da 5118 righe** (`mail_thread.py`): email-gateway + chatter + tracking + web-push + `Store` in un
  unico AbstractModel con ~15 context-key. Non splittabile: non puoi adottare la chatter senza il gateway email.
- **Tabella tracking sparsa** (~10 colonne tipizzate, una sola coppia non-null per riga); dispatch runtime stringly-typed.
- **Subtype come tabella mutabile** la cui ogni modifica invalida una cache globale; indirezione + join in più sull'hot path.
- **Fan-out notifiche**: una riga per destinatario per messaggio → tabelle enormi su record con molti follower + cron di GC.
- **Accoppiamento al client web**: `Store()`/bus serializzano un grafo Discuss-shaped — il layer dati non è usabile headless.
- **`activity_state` scritto 3 volte** (Python + 2 subquery SQL): tre fonti di verità per una regoletta.

## 4. Design Kigumi (più efficiente)

### (a) Opt-in via REGISTRY, non god-mixin
Un modello dichiara di essere "mailed" con un marker compile-time (side-registry come `register_acls!`/
`external_tables`): `register_mailed!("sale.order")` → `MailedRegistration { model }` + `is_mailed(model)`.
Il framework ITERA questo registry per: la pulizia alla cancellazione, il gating dell'API chatter (puoi postare
solo su modelli mailed), e (futuro) le viste. **Niente mixin da 5000 righe, niente `_inherit` a runtime.**

### (b) Modelli come normali `#[model]` Kigumi (crate `modules/mail`)
- `mail.message`: `res_model` Text, `res_id` Integer, `author_id` M2o(res.users), `message_type` Selection
  (comment/notification/note), `body` Text, `date` **Datetime**, `parent_id` M2o(mail.message) per il threading.
- `mail.activity`: `res_model`/`res_id`, `summary` Text, `date_deadline` **Date**, `user_id` M2o(res.users),
  `active` Bool. Lo **state è DERIVATO** dalla scadenza nell'API (overdue/today/planned), niente colonna né cron.
- `mail.follower`: `res_model`/`res_id`, `user_id` M2o(res.users), UNIQUE(res_model,res_id,user_id).
- `mail.tracking`: `message_id` M2o(mail.message), `field` Text, `old_value` Text, `new_value` Text (valore
  **serializzato dal `Value` tipizzato** — una sola coppia di colonne, non 10 sparse).

### (c) Fix dell'integrità polimorfica — IL punto chiave
Odoo non ha FK perché i record si cancellano per vie che bypassano l'ORM (SQL bulk, drop, uninstall). **Kigumi
ha UN SOLO path di delete** (`delete_secured`): aggiungo lì un **delete-cleanup hook** — quando si cancella un
record di un modello mailed, si cancellano le sue righe `mail.message`/`mail.activity`/`mail.follower`
(`WHERE res_model=? AND res_id=?`). Affidabile *perché* il path è unico (a differenza di Odoo). Più un indice
composito `(res_model, res_id)`. Niente FK polimorfica fasulla, niente GC cron, niente CHECK a mano.

### (d) Tracking che riusa il `Value` tipizzato
Un campo `#[field(tracked)]` → registry `TrackedFieldRegistration`. Nel write path (`update_secured`), per un
modello mailed, si diffano i valori vecchi/nuovi dei campi tracked e si crea UN messaggio `notification` con
righe `mail.tracking` (old/new serializzati dal `Value`). Niente `track_visibility` runtime, niente snapshot
precommit con chiavi f-string: il diff sfrutta il read del record già presente nel write path.

### (e) API chatter HEADLESS (niente Store/bus)
- `POST /api/:model/:id/message` (body, type?) → posta · `GET /api/:model/:id/messages` → thread (messaggi+tracking).
- `POST /api/:model/:id/follow` / `unfollow` · `GET /api/:model/:id/followers`.
- `POST /api/:model/:id/activity` (summary, date_deadline, user_id) · `GET /api/:model/:id/activities` ·
  `POST /api/mail.activity/:id/done`.
Tutto passa dal layer secured (post richiede Read sul record; ecc.). Risposta = JSON pulito, niente grafo Discuss.

### (f) DROP / DEFER (esplicito)
Gateway email in ingresso (`mail.alias`), bus/websocket/Discuss/`Store()`, web-push, **subtypes** (v1 = follow-all),
e le righe `mail.notification` per-destinatario (l'unread si deriverà da messaggi + un marker last-read per utente,
in una fetta successiva). Niente di tutto ciò serve a un ERP headless v1.

## 5. Piano a fette (incrementi spedibili)
1. **Fondazione** — `modules/mail` crate + `register_mailed!` (core registry) + modello `mail.message` + API
   `POST/GET .../message(s)` + **delete-cleanup hook** in `delete_secured`. File: `modules/mail`, `core/registry.rs`,
   `kigumi/src/lib.rs` (macro), `kigumi-db/src/lib.rs` (hook), `kigumi-server/src/lib.rs` (routes), `cli` (link).
2. **Tracking** — `#[field(tracked)]` registry + diff nel write path → messaggio `notification` + righe `mail.tracking`.
3. **Activities** — `mail.activity` + API schedule/done/list, state derivato.
4. **Followers** — `mail.follower` + follow/unfollow/list.
5. **FE chatter** — widget chatter nel form generico (thread + box messaggio + activities).
- **Retrofit**: marcare `sale.order` come mailed + tracciare `state`.

## 5b. Stato implementazione
- **Fetta 1 FATTA** (commit `4ce5eb0`): `register_mailed!` + `mail.message` + API chatter + delete-cleanup hook + `Db::now()`.
- **Fetta 2 FATTA**: `#[field(tracked)]` + `mail.tracking` + diff nel write path + tracking embedded nel thread.
  Il diff confronta old vs new entrambi resi da Postgres `::text` (re-SELECT del nuovo valore dopo l'UPDATE,
  riga lockata `FOR UPDATE`): niente falsi positivi su Date/Datetime/Float, niente old/new in formati diversi.
  Tracking = best-effort post-commit (non fa mai fallire la scrittura). Redazione D6: l'embedding del tracking
  nel thread filtra i campi non leggibili dal chiamante (`field_accessible`). Indici mail via `ensure_mail_indexes`.
- **Fetta 3 FATTA**: `mail.activity` (owner polimorfico, `user_id` int senza FK) + API
  `GET .../activities` · `POST .../activity` · `POST .../activities/:aid/done`. Lo **state è DERIVATO**
  (overdue/today/planned) in un solo punto (`activity_state`), confronto lessicale su date ISO. Per garantire
  l'invariante ISO il pool fissa `DateStyle = 'ISO, YMD'` su ogni connessione (`after_connect`) — vale anche
  per i `::text` di date/datetime usati altrove (tracking, FE). `done` verifica l'appartenenza all'host.
  Cleanup già coperto (mail_activity in THREAD_TABLES). Indurito contro review avversariale (7 finding: pin
  DateStyle, validazione `user_id`, deadline vuota = nessuna scadenza, `done` veritiero su 0 righe, helper
  `served_model` unificato).
- **Fetta 4 FATTA**: `mail.follower` (owner polimorfico, `user_id` int senza FK) + API
  `GET .../followers` · `POST .../follow` · `POST .../unfollow`. Idempotenza via indice UNICO composito
  `(res_model, res_id, user_id)` (`ensure_mail_indexes`): follow tollera il Conflict (23505) come successo,
  unfollow è no-op se non segui. **Sicurezza**: solo il gruppo `admin` può (un)seguire PER ALTRI utenti
  (`ensure_self_or_admin`) — un utente normale agisce solo su se stesso (fix IDOR da security review).
  Subtype/opt-in per-evento rimandati (v1 = follow-all). Preambolo auth+gate+modello dei chatter handler
  centralizzato in `chatter_setup` (un solo punto per la decisione d'accesso). Test idempotenza+cleanup.
- **Gap noti rimandati** (segnalati dalle review avversariali): paginazione del thread (verrà con la chatter FE,
  fetta 5); guard compile-time di `#[field(tracked)]` su campi relazionali/computed (oggi no-op silenzioso);
  quoting degli identificatori SQL per nomi-colonna riservati (es. `when`) — concerne tutto il metamodello, non solo mail;
  modello mail "served ma non migrato" → 500 invece di degradare (coerente con tutti i served model; non accade col migrate normale).

- **Fetta 5 FATTA (mail subsystem COMPLETO)**: widget chatter nel FE generico. Contract espone `mailed`
  (`to_ui_contract`); client tipizzato in `api.ts` (messages/post, activities/schedule/done, followers/
  follow/unfollow); componente `Chatter` (thread con commenti/note/audit-tracking, activities con badge di
  stato derivato + schedule/done, follow toggle + conteggio) montato da `ModelForm` per i record mailed.
  Indurito vs review FE (6 finding: gating `busy` su follow + done, stato di loading, helper `expectOk`,
  narrowing errori uniforme, rimozione `.replace` morto). `tsc --noEmit` pulito, build verde.

## 6. Raccomandazione + rischi
**Prima fetta**: la fondazione (registry opt-in + `mail.message` + post/list + cleanup hook) — è il minimo che
rende la chatter reale ed è dove vivono le decisioni architetturali. **Rischio principale**: il link polimorfico —
mitigato dal cleanup hook sull'unico delete path (forza di Kigumi) + indice composito. Secondario: l'API chatter
aggiunge route fuori dal pattern CRUD generico; vanno tenute coerenti col layer secured.
