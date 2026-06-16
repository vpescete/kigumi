# Meshble

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
  meshble-core     metamodello ispezionabile, risoluzione estensioni, manifest/versioning
  meshble-macros   proc-macro #[model] (fase 2)
  meshble-schema   proiezioni: DDL Postgres, contratto-UI JSON
  meshble          facade (prelude)
modules/
  sales            modulo d'esempio: sale.order + estensione sale_margin
docs/
  ANALISI_ODOO19.md   analisi del sorgente Odoo 19 (forze/debolezze)
  METAMODEL_DESIGN.md design del metamodello
  VERSIONING.md       versioning di framework e moduli
```

## Prova

```bash
cargo test
cargo run -p meshble-mod-sales --example demo
```

## Stato

Walking skeleton (fase 1): metamodello → DDL + contratto-UI, risoluzione estensioni con
check conflitti/`depends`, versioning moduli con SemVer verificato. Roadmap in
[`docs/METAMODEL_DESIGN.md`](docs/METAMODEL_DESIGN.md) e [`docs/VERSIONING.md`](docs/VERSIONING.md).

## Licenza

MIT OR Apache-2.0
