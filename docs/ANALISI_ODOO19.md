# Analisi architetturale di Odoo 19 Community

> Base di conoscenza per la rifondazione del framework (target: Rust, headless,
> agnostico, integrabile). Analisi verificata sul sorgente reale di Odoo 19.0
> (sparse checkout: `odoo/`, `odoo/addons/base/`, `addons/web/`).
> Riferimenti nel formato `file:riga`.

## Indice
1. [Sintesi esecutiva](#1-sintesi-esecutiva)
2. [Il ciclo di una request (end-to-end)](#2-il-ciclo-di-una-request-end-to-end)
3. [Backend / Server / Monolite](#3-backend--server--monolite)
4. [ORM](#4-orm)
5. [Sistema di moduli & data-as-code](#5-sistema-di-moduli--data-as-code)
6. [Frontend / web client (OWL)](#6-frontend--web-client-owl)
7. [Trasversali: security, reporting, automazione, i18n](#7-trasversali)
8. [La domanda "è monolitico?"](#8-la-domanda-è-monolitico)
9. [Sentiment community](#9-sentiment-community)
10. [Sintesi: cosa portare, cosa rifondare](#10-sintesi-cosa-portare-cosa-rifondare)

---

## 1. Sintesi esecutiva

**Il valore di Odoo** non è una tecnologia specifica: è il **metamodello dichiarativo**
(una definizione di modello genera DB + UI + API + security) unito all'**estendibilità
per composizione** (i moduli estendono altri moduli senza forkarli).

**Il difetto strutturale trasversale**: questa potenza è realizzata con **mutazione
runtime non tipizzata** — `type()` per costruire i modelli, `safe_eval` per dati/regole/
automazione, `xpath` per le viste, QWeb-XML per la UI — su un **runtime GIL-bound che
duplica lo stato** in ogni worker.

Le due leve che un linguaggio compilato (Rust) aggiunge:
- **Risoluzione a compile time** (type system + macro) → ispezionabilità, type-safety,
  validazione cross-modulo prima del deploy.
- **Concorrenza reale senza GIL** → un Registry condiviso invece di N copie in RAM.

---

## 2. Il ciclo di una request (end-to-end)

```
Browser (OWL)  ──POST /web/dataset/call_kw/sale.order/web_read (JSON-RPC)──►
WSGI Application.__call__()                                     odoo/http.py
  └─ _serve_db() → Registry(db)  (singleton in-memory per processo)
       └─ cursor READONLY + check_signaling()  (invalida cache se altri worker han scritto)
            └─ ir.http._match() → routing Werkzeug → endpoint
                 └─ se readonly: resta su replica; altrimenti cursor RW
                      └─ service.model.retrying() → Model.web_read()
                           └─ ORM: recordset, prefetch, record rules nel WHERE
                                └─ commit + signal_changes() (NOTIFY agli altri worker)
◄── JSON: il client OWL applica il diff al record reattivo
```

Tre invarianti ricorrenti:
1. Tutto passa da un **Registry in-memory per-database, per-processo** (cuore del monolite).
2. Coordinamento tra worker **via PostgreSQL** (`orm_signaling_*` + `LISTEN/NOTIFY`),
   nessuna dipendenza esterna, ma latenza di propagazione.
3. **Una transazione per request**, con retry automatico su serialization failure.

---

## 3. Backend / Server / Monolite

### Come funziona
- **Tre modelli di esecuzione** (`odoo/service/server.py`):
  - `ThreadedServer` (dev): un thread per request, **GIL-bound**.
  - `PreforkServer` (prod): master + N worker via `fork()` (copy-on-write), socket TCP
    condiviso, + worker cron dedicati che fanno `LISTEN cron_trigger`.
  - `GeventServer`: greenlet per longpolling/websocket (bus).
- **Registry per-database** (`odoo/orm/registry.py:113`): `Registry(db)` singleton con LRU
  cache globale; un processo serve N database, ~15 MB l'uno in RAM.
- **Connessioni** (`odoo/sql_db.py`): `ConnectionPool` (default 64) per processo, isolation
  **REPEATABLE READ**, pool separato per la read-replica.
- **Concorrenza** (`odoo/service/model.py` `retrying()`): fino a 5 retry, backoff esponenziale.
- **Split read/write nativo (novità 19)**: route con `readonly=True`
  (`addons/web/controllers/dataset.py:28`) → letture su replica, scritture sul primario.

### Forze
- Multi-tenant pulito; coordinamento zero-dipendenze (solo Postgres); retry gestito dal
  framework; read/write splitting pronto per le replica.

### Debolezze
- **GIL** → in prod servono prefork → **ogni worker duplica il Registry in RAM**.
- **Stato in-process**: cambio modello → tutti i worker ricaricano al prossimo request;
  niente hot-reload granulare.
- **Latenza signaling**: cache cross-worker invalidata solo a inizio request (~1-2s).
- **Una txn per request, no nested reale** (solo savepoint) → lock contention su request lunghe.
- **Cron via polling** (`SLEEP_INTERVAL = 60s`).
- Scala **solo per processi** (costo RAM per worker alto).

---

## 4. ORM

### Come funziona (`odoo/orm/`, ri-modularizzato in 19)
- **Recordset set-based**: `self` è sempre una collezione; `mapped`/`filtered`/`sorted`
  → operazioni batch.
- **Classe modello sintetizzata a runtime** (`odoo/orm/model_classes.py:179`):
  ```python
  model_cls = type(name, (model_def,), {...})   # la classe NON esiste in un file
  model_cls._base_classes__ = tuple(bases)       # :209 MRO da tutti gli _inherit
  ```
  La definizione "finale" di un modello è il prodotto di tutti i moduli che lo estendono,
  fusa al boot.
- **Campi dichiarativi** con `compute`/`store`/`related` + dependency graph (`@api.depends`),
  cache di prefetch automatica.
- **Tre eredità**: `_inherit` (estensione in-place), `_name` nuovo (prototype),
  `_inherits` (delegation).
- **Domini** (`odoo/orm/domains.py`, 2023 righe) come AST `[('field','op',val)]` → SQL.

### Forze
- Modello set-based (meno codice, batch by default); `self.env` unico (cursor + utente +
  contesto i18n/tz); estensibilità per composizione; riflessività (`ir.model.*`).

### Debolezze
- **Classe runtime non ispezionabile** → niente "go to definition", bug "stessa config,
  comportamento diverso".
- **Zero type-safety**; refactor = grep.
- **N+1 silenziosi**; ORM non ottimizzato per i bulk (audit: +30-50% solo refactorando l'ORM).
- **`onchange` nel layer UI** (`odoo/orm/models.py:6973`): gira dalla form, non garantito su
  ogni `create/write` via RPC → logica duplicata.
- **`create`/`write` override** come unico hook → metodi monstre, `super()` chain
  ordine-dipendenti.

---

## 5. Sistema di moduli & data-as-code

### Come funziona
- **`__manifest__.py`**: `depends`, `data` (ordine di caricamento), `assets`, `auto_install`,
  `application`, hooks.
- **Grafo dipendenze** (`odoo/modules/`): ordinamento topologico per depth+nome, `base`
  primo; caricamento sequenziale sotto lock globale.
- **Data-as-code** (`odoo/tools/convert.py`): XML `<record>`/CSV → ORM, **external ID**
  (`module.record_id`) via `ir.model.data`; idempotenza con
  `ON CONFLICT ... DO UPDATE WHERE NOT noupdate`; `eval=`/`ref=`/`search=` con `safe_eval`.
- **Security data-driven**: `ir.model.access.csv` (CRUD per gruppo) + `ir.rule` con
  `domain_force` (row-level, `safe_eval` a runtime, `odoo/addons/base/models/ir_rule.py:70`).

### Forze
- Convention over configuration; **external ID = idempotenza reale** (upgrade ripetibili,
  customization preservata con `noupdate`); manifest = package manager + loader dichiarativo.

### Debolezze
- **3-4 DSL per modulo** (Python/XML viste/XML data/CSV/JS-XML) **senza validazione
  cross-linguaggio** (un `ref` rotto esplode a install time).
- **Ordine di caricamento fragile**; nessuna risoluzione automatica delle dipendenze
  intra-modulo.
- **`auto_install` + glue modules** → esplosione combinatoria poco testata.
- **Nessun confine**: ogni modulo può sovrascrivere qualsiasi cosa.
- **`safe_eval` di stringhe** ovunque → superficie d'attacco.

---

## 6. Frontend / web client (OWL)

### Come funziona
- **OWL** (lib proprietaria, `addons/web/static/lib/owl/owl.js`): componenti reattivi, hooks,
  template QWeb-XML.
- **Registry extension point** (`addons/web/static/src/core/registry.js`): categorie `fields`,
  `views`, `services`, `actions`, `*_compilers`.
- **Services + DI** (`addons/web/static/src/env.js`): `orm`, `action`, `notification`, `rpc`,
  `bus`; `useService("orm")`.
- **View = Model/Renderer/Controller/ArchParser** (`addons/web/static/src/views/`): l'arch XML
  è parsato client-side in componenti OWL a runtime; `RelationalModel` gestisce dirty tracking
  + onchange round-trip.
- **ORM mirror client** (`addons/web/static/src/core/orm_service.js`): ogni metodo modello via
  `call_kw`; comandi x2many `(0,1,2,3,4,5,6)`.
- **Asset pipeline**: bundle nel manifest, compilati server-side (SCSS proprietario), serviti
  da `/web/bundle/{name}`; lazy bundle per graph/pivot.

### Forze
- Registry pluggabili + widget per-tipo → nuova schermata a costo marginale ~zero; OWL ha dato
  controllo totale (+40% perf vs vecchio web client); services con DI.

### Debolezze
- **OWL ecosistema isolato** (reinventa React/Vue: tooling, devtools, hiring); custom JS si
  rompe ad ogni cambio versione OWL.
- **QWeb-XML non type-checked**; il compiler manipola stringhe XML.
- **Pipeline asset proprietaria**: no tree-shaking serio, no HMR, no content-hash; bundle
  backend ~4-5MB; cambio manifest → restart/rebuild → ciclo di sviluppo lento.
- **Accoppiamento server↔client sulla semantica** (modelli/domini/onchange) → difficile avere
  client alternativi.
- **Onchange = round-trip ad ogni cambio campo**, niente optimistic update.

---

## 7. Trasversali

- **QWeb server-side** (`ir_qweb.py`): template → funzione Python generatore, cache a livello
  registry, output **auto-escaped** (anti-XSS). Elegante.
- **Reporting** (`ir_actions_report.py`): QWeb→HTML→PDF via **wkhtmltopdf** — **deprecato**
  (Qt WebKit abbandonato dal 2016, no flexbox/grid, problemi DPI).
- **Automazione**: `ir.cron` (eredita `ir.actions.server`, auto-deattivazione su failure) e
  server action `state='code'` → **`safe_eval` di stringhe Python** (injection se editabile).
- **i18n** (`odoo/tools/translate.py`): estrazione Babel da QWeb/`_()`/CSV → PO→MO; campi
  `translate=True`; **sync PO manuale**.

---

## 8. La domanda "è monolitico?"

Tre assi distinti:

| Asse | Monolitico? | Conseguenza | Nel rebuild |
|---|---|---|---|
| **Deployment** (un processo/binario) | Sì | Semplice da operare; scala per processi (RAM/worker per il GIL) | Rust async: un processo, tutti i core, Registry condiviso |
| **Runtime/stato** (Registry in-memory per processo) | Sì | N copie in RAM, no hot-reload, signaling latente | `Arc<Registry>` condiviso, invalidazione granulare |
| **Codebase/estensione** (tutto estende tutto) | Sì (by design) | Ecosistema potente / nessun confine, no type-safety | Estensione componibile **con confini dichiarati e tipizzati** |

La "monoliticità di deployment" è un **pregio operativo** (zero dipendenze, un binario). I
problemi veri sono **il GIL** (forza la duplicazione RAM) e **il codebase senza confini né tipi**.

---

## 9. Sentiment community

**Eccelle**: all-in-one / dati centralizzati; modularità + low-code (Studio); open-source +
evoluzione continua; OWL come scommessa di controllo (+40% perf); backward-compatibility.

**Da migliorare (FRAMEWORK)** — corrispondono 1:1 ai riscontri sul sorgente:
- N+1 da "ORM misuse", non ottimizzato per bulk (audit +30-50%).
- "Rimuovere elementi da template rompe altri moduli"; XPath che non matcha → `apply_inheritance_specs`/`locate_node` (`ir_ui_view.py:944`).
- Custom JS si rompe ad ogni versione OWL.
- Ciclo di sviluppo lento (reload modulo, no HMR).
- wkhtmltopdf obsoleto.
- Codice poco documentato, "devi leggere il codebase"; istanze identiche si comportano diversamente (effetto della classe runtime).

**Da migliorare (PRODOTTO/BUSINESS, ortogonali al rebuild)**: supporto frammentato/sales-oriented;
docs/tutorial datati; scalabilità enterprise; costi di implementazione.

Fonti principali: forum Odoo ("Rant: developing with Odoo"), dev.to (ORM misuse), Medium
(migrazione OWL), Capterra/Clinked/Gloriumtech (review).

---

## 10. Sintesi: cosa portare, cosa rifondare

| Componente | Forza da PRESERVARE | Debolezza da RIFONDARE |
|---|---|---|
| **Server** | multi-tenant, signaling zero-deps, read/write split | GIL→duplicazione RAM, no hot-reload, una-txn-per-request |
| **ORM** | recordset set-based, env unico, campi dichiarativi | classe runtime non ispezionabile, no type-safety, N+1, onchange in UI |
| **Moduli** | manifest dichiarativo, external ID idempotente, convention | multi-DSL non validato, ordine fragile, nessun confine, `safe_eval` |
| **Frontend** | registry pluggabili, widget per-tipo, UI a costo ~zero | OWL isolato, no HMR/tree-shaking, accoppiamento semantico server-client |
| **Trasversali** | QWeb auto-escaped, security data-driven (ACL+RLS) | wkhtmltopdf morto, automazione via eval, i18n sync manuale |

→ Le scelte di design che ne derivano sono in [`METAMODEL_DESIGN.md`](./METAMODEL_DESIGN.md).
