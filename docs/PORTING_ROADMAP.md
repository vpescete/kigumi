# Roadmap: portare i moduli Odoo Community core su Kigumi

> Analisi grounded sul sorgente Odoo 19 (`odoo19/addons/{mail,product,uom,stock,sale,purchase,account,…}`)
> + inventario delle primitive Kigumi attuali. Obiettivo: portare la **catena business core**, non
> "tutto Community" (il grosso del numero di moduli — `l10n_*`, temi, integrazioni — è basso valore).

## Principio guida

Il collo di bottiglia **non è scrivere i modelli, sono le PRIMITIVE di framework mancanti**. Quasi
ogni modello Odoo usa cose che Kigumi oggi non ha. Quindi: **prima le primitive moltiplicatrici, poi
i moduli in ordine di dipendenza, in profondità su una catena** (non largo-e-superficiale).

## 1. Gap-matrix delle primitive

### Tipi di campo

| Primitiva | Kigumi | Priorità | Note |
|---|---|---|---|
| `Date` / `Datetime` | **MANCA** | 🔴 sblocca-tutto | nessun tipo temporale; quasi ogni modello ne ha |
| `Float` (non-monetario) | **MANCA** | 🔴 alta | abbiamo solo `Decimal{currency_field}`; quantità/pesi/misure servono float con `digits` |
| `Many2many` di prima classe | **MANCA** (D1=junction) | 🔴 alta | attributi prodotto, tag, route stock — ovunque |
| `related` (campo correlato) | **MANCA** | 🔴 alta | pervasivo (es. `currency_id` da `company_id`) |
| `Html` | **MANCA** | 🟠 media | descrizioni, note, mail |
| `Binary` / `Image` | **MANCA** | 🟠 media | immagini prodotto, avatar, firme, allegati |
| `Many2oneReference` (polimorfico) | **MANCA** | 🟠 media | serve SOLO a mail (un'unica tabella message/activity per tutti i modelli) |
| `Properties` / `Json` dinamico | **MANCA** | 🟢 bassa | campi dinamici per-categoria; rimandabile |
| `Char` vs `Text` | PARZIALE | 🟢 bassa | `Text` fa entrambi; manca solo length/single-line |

### Meccanismi

| Primitiva | Kigumi | Priorità | Note |
|---|---|---|---|
| compute non-stored (on-read) | **MANCA** | 🔴 alta | abbiamo solo compute stored; molti campi Odoo sono calcolati al volo |
| `related` fields | **MANCA** | 🔴 alta | (vedi sopra) |
| **mail/chatter/activities + mixin** | **MANCA** | 🔴 il grosso | `mail.thread`+`mail.activity.mixin`: quasi OGNI modulo business li eredita |
| `_inherits` delegation inheritance | **MANCA** | 🔴 alta (product) | `product.product` _inherits `product.template` (variant) |
| onchange | **MANCA** | 🟠 media | Odoo 19 ne ha migrati molti a compute → meno critico |
| `@api.constrains` (Python) | **MANCA** (solo SQL CHECK) | 🟠 media | vincoli cross-record (es. partita doppia bilanciata) |
| report QWeb/PDF | **MANCA** | 🟠 media | quotazioni, PO, fatture, etichette |
| cron / azioni schedulate | **MANCA** | 🟠 media | scheduler stock, coda mail |
| wizard / transient models | **MANCA** | 🟠 media | conferme backorder, advance invoice, ecc. |
| tracking (audit dei campi) | **MANCA** | 🟢 bassa | parte di mail |
| viste kanban/pivot/calendar/graph | **MANCA** (FE) | 🟢 bassa | per ora abbiamo list/form generici contract-driven |

### Cosa Kigumi HA GIÀ (e che spesso è migliore di Odoo)

Metamodello ispezionabile + DDL generato; compute stored same-record + **aggregati con cascata
multi-livello**; x2many con **comandi tipizzati** `{op,id,values}` (≠ tuple posizionali Odoo); azioni
di transizione stato; `ir.sequence` gapless; **ACL + record rules** (anche **DB-backed**) con Domain
tipizzato → **SQL parametrizzato** (mai `safe_eval`); **multi-company** completo con default-deny;
**field-level security** (D6); **selezione moduli** con risoluzione dipendenze SemVer verificata;
`#[extend]` (merge verificato, non monkey-patch); money esatto (`rust_decimal`).

## 2. Tier dei moduli (ordine di dipendenza)

**Tier 0 — primitive (da costruire PRIMA):**
- P0a: `Date`/`Datetime`, `Float`, `related` — **S** (piccoli, moltiplicatori)
- P0b: `Many2many` di prima classe — **M**
- P0c: **mail subsystem** (mixin opt-in + `mail.thread`/`mail.message`/`mail.activity`/`mail.followers`
  + `Many2oneReference` + tracking) — **XL** (il cornerstone; `mail_thread.py` da solo ~257KB)
- P0d: `Html`/`Image`, compute non-stored, `@api.constrains`, report, cron, wizard — **L** (a ondate)

**Tier 1 — catena business:**
- `product` + `uom`: template/variant (richiede **`_inherits`**), attributi (**M2M**), `Float`+`digits`,
  `Date`, `Image`, pricelist engine, category tree (`_parent_store`) — **L**
- `stock`: il più pesante — quant reservation engine, state machine 7 stati su `stock.move`, scheduler
  (**cron**), pull/push routing, **wizard**, **report** — **XL**
- `sale` (completare) + `purchase`: Kigumi ha già `sale.order`/`line` (7 campi); Odoo è molto più ricco
  (motore tasse delegato ad `account`, invoicing engine, `mail.thread`, report, doppia validazione PO,
  quotation template) — **M/L ciascuno**

**Tier 2 — più avanti:**
- `account` (contabilità): il più profondo/regolato (`account.move`+`account.move.line`, piano dei conti,
  tasse, giornali, pagamenti, partita doppia bilanciata) — **XL**
- `crm`, `project`, `hr` (richiedono mail) — **M ciascuno**

## 3. Sequenza consigliata (incrementi spedibili)

1. **Fase A — moltiplicatori** (`Date`/`Datetime`, `Float`, `related`): tocca `FieldKind`+macro+schema
   +domain+db. Piccola, sblocca subito decine di campi. → spedibile.
2. **Fase B — `Many2many`** di prima classe (tabella di join generata + comandi x2many estesi).
3. **Fase C — `product` "pragmatico"** (senza chatter): template/variant via `_inherits`, attributi via
   M2M, prezzi/UoM. Primo modulo business "vero" e radice della catena.
4. **Fase D — mail subsystem** (milestone dedicata): mixin + chatter/activities + `Many2oneReference` +
   tracking. Poi **retrofit** del chatter su `product`/`sale`.
5. **Fase E — `sale`/`purchase` completi** (su mail + report).
6. **Fase F+ — `stock`, poi `account`** (i due "mostri", quando il framework è ricco).

## 4. Cosa NON portare ora

`l10n_*` (localizzazioni), temi, integrazioni, `point_of_sale`/`website` (verticali enormi a sé),
`account` rimandato a Tier 2, e tutto il long tail. È il grosso del *numero* di moduli ma il minor
valore per la fase attuale.

## 5. Stima e raccomandazione

La catena core (`mail`+`product`+`stock`+`sale`+`purchase`+`account`) è un lavoro **grosso** (mesi di
lavoro-uomo equivalente, incrementale). I due **mostri** sono **`mail`** (cornerstone trasversale) e
**`account`** (profondità contabile).

**Decisione strategica chiave — la fedeltà del port dipende da `mail`:**
- **(a) mail-first**: costruisci mail prima → ogni modulo dopo è fedele (chatter/activities) ma l'upfront
  è XL.
- **(b) pragmatic-first**: porta `product`/`sale` SENZA chatter ora (mail come retrofit) → valore spedito
  prima, ma quei moduli non hanno chatter finché mail non arriva.

**Raccomandazione**: **primitive moltiplicatrici (Fase A) → `product` pragmatico (Fase C) → mail come
milestone dedicata → retrofit**. Rimandare `stock` e `account` finché il framework non ha report/cron/
wizard. Il **next step a più alta leva** è la Fase A (Date/Datetime + Float + related): piccola, e
moltiplica immediatamente ciò che possiamo portare.
