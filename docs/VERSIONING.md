# Versioning — framework & moduli

> Come Odoo gestisce le versioni, cosa non funziona, e il modello che adottiamo.
> Vedi anche [`ANALISI_ODOO19.md`](./ANALISI_ODOO19.md) e [`METAMODEL_DESIGN.md`](./METAMODEL_DESIGN.md).

## 1. Come fa Odoo (e perché è fragile)

- **Framework**: serie annuali `16.0 → 17.0 → 18.0 → 19.0`. `version_info = (19, 0, 0, FINAL, 0)`.
  Ogni major è una "serie"; le serie non sono compatibili tra loro.
- **Modulo**: campo `version` nel `__manifest__.py`. Convenzione a 5 numeri
  `19.0.1.0.0` = `<serie_odoo>.<major>.<minor>.<patch>`. Se ometti il prefisso, Odoo
  antepone la serie → **lockstep**: ogni modulo è legato a una serie di Odoo.
- **Dipendenze**: `depends = ['sale', 'stock']` — **senza alcun vincolo di versione**. Un
  modulo non può dire "mi serve sale ≥ 2.3". Si rompe a runtime se l'API cambia.
- **Migrazioni**: script in `migrations/<version>/{pre,post,end}-*.py`, eseguiti quando la
  `version` nel manifest supera quella installata in `ir_module_module`. Sono Python liberi.

**Problemi**: lockstep serie↔modulo (re-release forzata ad ogni major), depends senza
versione (nessuna garanzia di compatibilità), migrazioni come script imperativi non tipizzati.

## 2. Il modello Kigumi

### 2.1 Due piani distinti: catalogo (compile time) vs attivazione (runtime)

L'intuizione chiave che concilia "type-safety a compile time" e "install/uninstall per
tenant" di Odoo:

- **Catalogo dei moduli = compile time.** Tutti i moduli disponibili sono crate Rust,
  linkati nel binario, **risolti e type-checked insieme** (composizione verificata, niente
  `_inherit` opaco). `cargo` ci dà gratis: SemVer, range di dipendenza, build riproducibili.
- **Set installato = runtime, per database.** Quali moduli sono *attivi* per un tenant è un
  **dato** (una riga per modulo per DB, con `installed_version`), non una ricompilazione.
  Esattamente come `ir_module_module.state` in Odoo, ma sopra un catalogo tipizzato.

Risultato: install/disinstall/upgrade per-tenant a runtime **senza** perdere le garanzie di
compile time. È il meglio dei due mondi.

### 2.2 Versione del framework

- **SemVer puro**, Cargo-native. Oggi `0.1.0` (workspace `Cargo.toml`). Esposta come
  `kigumi_core::FRAMEWORK_VERSION`.
- Pre-1.0: minor = possibili breaking. Dopo 1.0: major = breaking, minor = additivo, patch = fix.
- **Stabilità community**: a 1.0 si dichiara un contratto di stabilità del metamodello e delle
  API pubbliche; branch **LTS** per major con supporto pluriennale (analogo alle serie Odoo,
  ma senza forzare il lockstep dei moduli).

### 2.3 Versione e manifest del modulo

Ogni modulo dichiara (`ModuleManifest` in `kigumi-core`, oggi usato da `modules/sales`):

```rust
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "sales",
    version: "1.0.0",                 // SemVer del modulo, INDIPENDENTE dal framework
    framework: ">=0.1, <0.2",         // range di compatibilità col framework (VERIFICATO)
    depends: &[ModuleDep { name: "base", req: "^0.1" }],   // dep con RANGE SemVer
    summary: "Gestione ordini di vendita",
};
```

Differenze con Odoo, tutte verificabili:
- **Niente lockstep**: la versione del modulo (`1.0.0`) non incorpora la serie del framework.
  La compatibilità è un **range esplicito** (`framework: ">=0.1, <0.2"`), controllato da
  `check_compat()` → un modulo fuori range è un **errore**, non un crash a runtime.
- **Dipendenze con versione**: `ModuleDep.req` è un range SemVer → un resolver può rifiutare
  combinazioni incompatibili a install time (fase 3). Odoo qui non ha nulla.

### 2.4 Migrazioni (fasi successive)

- **Schema**: generate e diffabili dal `ResolvedModel` (DDL versionato), non scritte a mano.
- **Dati**: passi versionati `migrations/<from>-><to>/` come funzioni Rust tipizzate
  (`fn(ctx) -> Result<()>`), con hook `pre` / `post`, eseguiti quando
  `installed_version < manifest.version` per quel DB. Tipizzati, non `safe_eval` di stringhe.
- **External ID** (idempotenza dei dati seed) mantenuto: tabella equivalente a `ir_model_data`.

## 3. Confronto

| Aspetto | Odoo 19 | Kigumi |
|---|---|---|
| Versione framework | serie `19.0` | SemVer (`0.1.0`), LTS a 1.0 |
| Versione modulo | lockstep `19.0.x.y.z` | SemVer indipendente + range di compat |
| Compat framework↔modulo | implicita (per serie) | range **verificato** (`check_compat`) |
| Dipendenze tra moduli | nomi, **senza versione** | range SemVer (resolver, fase 3) |
| Install/uninstall per tenant | runtime (`ir_module_module`) | runtime su catalogo compile-time |
| Migrazioni | script Python per versione | passi Rust tipizzati + DDL generato |

## 4. Policy sulle pre-release

Una build pre-release del framework o di un modulo (es. `0.1.5-rc.1`) è trattata come la sua
**release line** (`0.1.5`) quando si confrontano i range. Senza questa policy, le regole
Cargo/SemVer rifiuterebbero ogni pre-release in-range (un range matcha una pre-release solo se
un comparator condivide l'esatto `major.minor.patch` e porta a sua volta una pre-release) →
ogni install fallirebbe durante le build RC/dev del framework stesso. Implementato in
`release_of()` e applicato sia alla compat col framework sia ai range tra moduli. La boundary
resta corretta: `0.2.0-rc.1` → `0.2.0`, ancora fuori da `<0.2`.

## 5. Questione aperta: policy sui nomi dei moduli

Oggi i nomi sono confrontati case-sensitive. Essendo literal Rust controllati, una collisione
case-only è improbabile (≠ Odoo, dove l'identità è la directory dell'addon). Da decidere se
imporre un **pattern canonico** (es. `snake_case` minuscolo) a registrazione + un hint
"did you mean" sui `MissingDependency`. Rimandato finché non c'è un ecosistema reale.

## 6. Stato attuale

Implementato e testato (fase 3 completa):
- `FRAMEWORK_VERSION` esposto dal core; `ModuleManifest` / `ModuleDep`.
- `check_compat()` con `semver` reale (range verificati, policy pre-release).
- **`resolve_module_set()`** (puro, testabile): compat framework + range dei `depends` +
  no-duplicati + no-self-dep + no-cicli → **ordine topologico**. Errori dedicati
  (`MissingDependency`, `DependencyConflict`, `DuplicateModule`, `SelfDependency`,
  `DependencyCycle` con i soli membri reali del ciclo).
- **`resolve_modules()`** + `register_module!` + `inventory`: auto-registrazione del catalogo.
- `modules/base` (`res.partner`, `res.currency`) + `modules/sales` (`depends: base`).
- Indurito da audit adversarial (4 fix: pre-release ×2, ciclo preciso, self-dep). 21 test verdi.

Roadmap versioning:
1. **Tabella `installed_module`** per-DB con `installed_version` (attivazione runtime).
2. **Motore di migrazioni** schema+dati versionato.
3. (Eventuale) policy sui nomi (§5).
