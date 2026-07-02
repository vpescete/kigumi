# Mini-design: `_inherits` (ereditarietà per delega) per Kigumi

> Obiettivo: il meccanismo che serve a `product.product` (variante) per esporre trasparentemente i
> campi di `product.template` (prodotto), come Odoo `_inherits`. Tenere i punti di forza, evitare le
> debolezze, sfruttando il metamodello tipizzato + l'unico path read/write controllato di Kigumi.
> Codice in inglese; prosa in italiano. Prerequisito per "product completo" (vedi PORTING_ROADMAP).

## 1. Come fa Odoo (`_inherits`, delegation inheritance)
Un modello figlio dichiara `_inherits = {'product.template': 'product_tmpl_id'}`: ha un FK **required**
`product_tmpl_id` verso il padre e **espone trasparentemente TUTTI i campi del padre**. Leggere
`product.name` legge `product.product_tmpl_id.name`; scrivere `product.name` scrive sul `product.template`
puntato. In `create`, se il FK non è fornito, Odoo **auto-crea** il record padre. È *composizione con
accesso-campo trasparente* — diverso da `_inherit` (estensione dello stesso modello/tabella, già coperto
da Kigumi con `#[extend]`) e dall'ereditarietà classica. `product.template` tiene i campi condivisi
(name, list_price, categ_id, uom, type…), `product.product` la variante (default_code, barcode, gli
`attribute_value_ids`); più varianti condividono un template.

## 2. Punti di forza da TENERE
- **Niente duplicazione**: i campi condivisi vivono in UNA tabella (`product_template`); N varianti li
  condividono senza copiarli. Niente sync template→varianti.
- **Accesso trasparente**: il consumer vede `product.product` come un modello unico (name, price…),
  ignorando lo split — ergonomia ottima per API/UI/domini.
- **Una sola riga di dichiarazione** per ottenere la delega — riusabile (anche `res.users` _inherits
  `res.partner` in Odoo).
- **FK reale** col padre (a differenza del link polimorfico del mail): integrità garantita dal DB.

## 3. Debolezze da EVITARE
- **Risoluzione dei campi a runtime + magia implicita**: in Odoo la delega è risolta dinamicamente
  (`_inherits_check`, `_add_inherited_fields`), con ordine di risoluzione e override sottili; un campo del
  padre e uno del figlio con lo stesso nome creano ambiguità silenziose.
- **Auto-create del padre opaco**: `create` su un figlio senza il FK crea silenziosamente un padre — utile
  ma può generare padri orfani/duplicati se l'API non è chiara su chi possiede il ciclo di vita.
- **`unlink` non simmetrico**: cancellare la variante NON cancella il template (può restare orfano);
  cancellare il template fallisce/cascata in modo dipendente dalla config FK.
- **Scrittura "split" implicita**: una `write` con campi misti (figlio+delegati) deve spezzarsi in due
  UPDATE (figlio + padre) in modo atomico — in Odoo è dentro l'ORM, difficile da ispezionare.
- **Vincoli/ACL/regole sui campi delegati**: un `required`/`unique` su un campo del padre vive sulla
  tabella del padre, non del figlio; le regole record del figlio non vedono di default i campi del padre.

## 4. Design Kigumi (più esplicito e verificabile)

### (a) Dichiarazione tipizzata, compile-time
`#[model(name = "product.product", table = "product_product", inherits = "product.template" via "product_tmpl_id")]`.
→ `ModelDescriptor` guadagna `inherits: Option<InheritsDecl { parent: &'static str, via: &'static str }>`.
Il `via` è un campo `Many2one(parent)` **required**, generato implicitamente (una colonna FK reale sul
figlio, con `ON DELETE` esplicito — vedi (e)). Niente magia runtime: la delega è nota a compile-time.

### (b) Risoluzione con PROVENIENZA esplicita
`resolve` (con accesso al padre risolto) produce un `ResolvedModel` i cui `fields` includono:
1. i campi PROPRI del figlio (colonne sulla sua tabella);
2. il campo FK `via` (colonna reale, Many2one→parent, required);
3. i campi DELEGATI del padre — copiati con un marcatore `delegated_via: Option<(parent_table, via_fk)>`
   (default `None`). `has_column()` resta `false` per i delegati → **nessuna colonna sul figlio** (la DDL
   non li emette; vivono sul padre). Per v1 si delegano solo le **colonne stored scalari** del padre
   (Text/Integer/Float/Decimal/Bool/Date/Datetime/Selection/Many2one); computed/One2many/Many2many del
   padre sono RIMANDATI (note). **Conflitto di nome** figlio↔padre = errore di risoluzione (no override
   silenzioso, a differenza di Odoo).

### (c) Read: subquery correlata sul padre (come i `related`)
`select_columns` per un campo delegato emette `(SELECT p.<col> FROM <parent_table> p WHERE p.id =
<child>.<via>)` — esattamente il pattern già usato per i campi `related`/Many2many. Niente JOIN che
moltiplica righe; una subquery per campo (o un JOIN singolo ottimizzabile poi). Riuso del meccanismo
esistente = poco codice nuovo nel read path.

### (d) Write: split atomico nello stesso TX
`insert/update_secured` partizionano il payload: chiavi proprie/`via` → tabella figlio; chiavi delegate →
`UPDATE <parent_table> SET … WHERE id = <child>.<via>` nello STESSO TX. **Create**: se `via` non è
fornito, si crea prima il padre con i campi delegati (un `insert` sul padre), si ottiene il suo id, lo si
mette in `via`, poi si crea il figlio — tutto in un TX (niente padre orfano se il figlio fallisce). Path
unico e ispezionabile (forza Kigumi vs Odoo).

### (e) Delete, vincoli, ACL/regole, contratto, domini
- **Delete**: `via` FK con `ON DELETE RESTRICT` di default (cancellare un template ancora referenziato da
  varianti fallisce con errore chiaro); cancellare la variante NON tocca il template (no auto-unlink — più
  prevedibile di Odoo). Un'azione esplicita potrà cancellare template+varianti insieme se serve.
- **Vincoli**: `required`/`unique`/`check` su un campo delegato restano sulla DDL del PADRE (la sua tabella
  li ha già); il write-split li rispetta via gli errori del padre mappati a BadInput.
- **ACL/regole/D6**: scrivere/leggere un delegato richiede l'accesso sul FIGLIO (è il modello che il
  consumer usa); i field-groups del delegato si ereditano dal padre (lookup su `parent.field`). Le regole
  record del figlio che filtrano su un delegato traversano il `via` come una normale relazione (`via.col`).
- **Contratto UI**: `to_ui_contract` espone i campi delegati come campi normali (read/write trasparente),
  così il form generico li mostra senza sapere dello split. Flag `inherits` esposto per debug.
- **Domini**: filtrare su un delegato = traversata `via.col` (già supportata dal domain→SQL). Opzionale:
  consentire il nome diretto del delegato come alias di `via.col`.

## 5. Piano a fette
1. **Dichiarazione + risoluzione + DDL**: `InheritsDecl` nel metamodello + macro `inherits = … via …` +
   `resolve` che inietta `via` FK + i campi delegati (marcati) dal padre risolto + conflitto=errore; DDL
   emette la colonna `via` (FK con ON DELETE RESTRICT) e NON i delegati. Test: risoluzione + DDL.
2. **Read**: `select_columns` → subquery correlata per i delegati (riuso pattern related). Test live.
3. **Write/Create**: split del payload + create-parent-first nello stesso TX. Test live (create con
   auto-padre, update misto, update del solo delegato).
4. **Contratto UI + domini + D6** sui delegati. 
5. **`product.template`/`product.product`**: ridefinire product come template (campi condivisi) + variante
   (`_inherits`), migrare il flat attuale; poi (futuro) attributi/varianti generate.

## 6. Raccomandazione + rischi
**Prima fetta**: dichiarazione+risoluzione+DDL — è dove vivono le decisioni (provenienza, conflitti, FK).
Poi read, poi write/create (la più delicata: split atomico + auto-create). **Rischio principale**: il
write-split tocca il path già complesso (computi, nested, m2m, company-scope, sicurezza) — mitigato
isolando i delegati PRIMA degli altri step e riusando il TX esistente. Secondario: i campi delegati
computed/relazionali del padre (rimandati) — un padre con un campo computed non sarà delegabile in v1
(errore di risoluzione chiaro, non silenzioso). **Decisione aperta**: `_inherits` è un meccanismo core
nuovo; questo design lo rende esplicito/tipizzato invece che runtime-magic. Se l'owner preferisce un
`product` "appiattito" senza varianti per la v1, si può rimandare l'intero meccanismo e arricchire il
`product.product` flat attuale — ma la roadmap chiede template/variant, quindi procedo col design sopra.

## 7. Meccanismo validato dalla review avversariale (29 finding: 6 critici, 9 high, 12 medium, 2 low)
La direzione del design è confermata corretta; la review ha precisato il MECCANISMO concreto e ha mostrato
che `_inherits` è una feature core INVASIVA (tocca ogni data path). Risoluzioni adottate:

- **Iniezione in `resolve_registered`, non in `resolve`** (#2,#6,#14,#19): `resolve(base,ext)` resta
  parent-agnostico. `resolve_registered(child)` risolve il padre via `resolve_registered(parent)` con
  **visited-set + depth-cap** (ciclo `_inherits` → ResolutionError chiaro, come `migration_plan`), poi
  inietta i campi delegati. Errore chiaro se il padre non è registrato/non risolve.
- **Predicato di delega** (#23): delega un campo del padre SSE `has_column()==true && related_path(parent,f).is_none() && !is_computed()`
  (solo colonne stored scalari). Computed/related/o2m/m2m del padre = NON delegati (v1).
- **Conflitto nomi DOPO il merge** (#22,#28): il check anti-collisione gira in `resolve_registered` dopo le
  estensioni + l'aggiunta del `via`; collisione = errore a startup (non "compile-time": il set padre è
  dinamico via inventory).
- **Read = riuso del meccanismo `related`** (#3,#5,#26 — l'opzione genuinamente LEAN): un campo delegato
  legge via subquery correlata `(SELECT p.col FROM parent p WHERE p.id = child.via)`, identica ai `related`.
  Provenienza esplicita (NON un flag generico su `FieldDef`): `select_columns`/`row_to_json`/`to_ui_contract`/
  order-by/filter ottengono un ramo `delegated_via(field)` parallelo a quello `related`.
- **Write = ramo dedicato** (#1,#4,#12,#15,#24): `split_nested` estrae le chiavi delegate in un
  `delegated: Map`, validate/cast/company-scope contro il modello PADRE; route a `UPDATE parent SET … WHERE
  id = child.via`. **Create**: estrarre `create_in_tx(model, …, &mut tx)` condiviso; se `via` assente,
  creare prima il padre (secured, stesso TX) e iniettare l'id in `via` PRIMA del check `require_all` del
  figlio (#21). Auto-create del padre SOLO se `via` assente (#17).
- **Via-FK dichiarato dall'autore** come normale `Many2one(parent) required` → colonna FK reale, **nessun
  churn su `FieldDef`** (che è costruito in decine di literal nei test). `inherits` registra solo `(parent, via)`.
- **D6 sui delegati** (#11,#20): `field_accessible`/`field_required_groups` diventano delegation-aware
  (fallback su `(parent, field)`). Company-scope del padre: decidere se `product.template` è company-scoped
  (probabilmente NO — catalogo condiviso) → niente company_id sul template in v1.
- **ON DELETE** (#9,#10,#25,#29): la `REFERENCES` attuale SENZA `ON DELETE` dà già il comportamento
  **RESTRICT/NO ACTION** di default → cancellare un template referenziato fallisce e mappa a
  `DbError::Conflict` (23503). Per v1 NESSUNA modifica DDL serve; `ON DELETE` esplicito rimandato.
- **Slice 1 trimmabile** (#29): essenziale = `InheritsDecl` + risoluzione (iniezione delegati + guard +
  conflitto) + DDL (via-FK colonna, delegati senza colonna) + read-via-related. Tagliabili da slice 1:
  ON DELETE esplicito, azione cascade-delete, alias-dominio del nome delegato.

**Conclusione sullo scope**: il design è pronto da implementare ed è la via giusta per template/variant.
MA è una feature core multi-fetta e invasiva (il write-split tocca il path più complesso: computi/nested/
m2m/company-scope/sicurezza). Esiste un'alternativa lean (product appiattito, varianti rimandate). È una
decisione di scope/effort dell'owner — vedi la domanda posta in chat.
