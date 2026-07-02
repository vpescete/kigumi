# Kigumi

Un framework applicativo **headless, schema-driven** in Rust — la base di Odoo
(metamodello dichiarativo + estendibilità per composizione) ricostruita partendo dai suoi
punti di forza e correggendo i punti deboli.

**Principi**: agnostico (il core non impone frontend né protocollo), integrabile (tutto
esposto via standard generati dallo schema), community-friendly (stack mainstream).

## Idea in una frase

Una sola **definizione di modello** è l'unica sorgente di verità; da essa si **generano** a
build time: schema DB, API (OpenAPI/GraphQL), contratto-UI agnostico (JSON), policy di
security. A differenza di Odoo, la composizione dei moduli è **risolta e verificata a compile
time**, non mutata a runtime.

## Workspace

```
crates/
  kigumi-core     metamodello ispezionabile, domini AST, security (ACL+rule+sudo), versioning
  kigumi-macros   proc-macro #[model] / #[extend]
  kigumi-schema   proiezioni: DDL Postgres, contratto-UI JSON, OpenAPI 3.1
  kigumi-db       persistenza Postgres (sqlx): CRUD security-enforced + migrazioni versionate
  kigumi-auth     auth JWT HS256 (Bearer → Ctx fidato)
  kigumi-server   server axum headless: metadata + CRUD dal catalogo
  kigumi          facade (prelude)
modules/
  base             res.partner, res.currency
  sales            sale.order + estensione sale_margin
apps/
  renderer-demo    demo eseguibile: migra+seed un modello, serve API + renderer
webui/
  app.html         renderer di riferimento (HTML+JS vanilla, generico, guidato dal contratto)
docs/               ANALISI_ODOO19 · METAMODEL_DESIGN · VERSIONING
```

## Prova

```bash
cargo test                                   # unit; gli integration-test DB si auto-skippano

# stack completo end-to-end (richiede un Postgres):
export DATABASE_URL=postgres://USER@127.0.0.1/kigumi_test
cargo test                                   # ora include gli integration-test live
cargo run -p kigumi-renderer-demo           # poi apri l'URL stampato (con token JWT)
```

Il demo migra un modello `task`, lo seeda, e serve su `:8099` sia l'**API headless**
(`/openapi.json`, `/api/models`, `/api/{m}/view`, CRUD `/api/{m}`) sia il **renderer**: un
frontend generico che disegna form e tabella *dal contratto*, autenticato via JWT.

## Stato

Slice verticale sicura completa (fasi 1-6): metamodello → DDL + migrazioni → SQL parametrizzato
→ CRUD security-enforced (ACL + record rules) → OpenAPI/contratto-UI → auth JWT → renderer
agnostico. Ogni pezzo security-critical passato per audit adversarial. Roadmap e dettagli in
[`docs/METAMODEL_DESIGN.md`](docs/METAMODEL_DESIGN.md) e [`docs/VERSIONING.md`](docs/VERSIONING.md).

## Licenza

MIT OR Apache-2.0
