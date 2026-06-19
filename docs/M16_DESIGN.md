# M16 — Account (double-entry general ledger)

The first XL business module: a headless double-entry GL. It cashes in two primitives built earlier —
`@api.constrains` (M11, the balanced-entry invariant) and the invoicing seam (M15.3) — and reuses
sequences (M2), mail/chatter (M11 retrofit), and the secured CRUD + action machinery.

## Models

| Model | Table | Key fields | Notes |
|---|---|---|---|
| `account.account` | `account_account` | `code` (req), `name` (req), `account_type` Selection, `reconcile` Bool, `company_id` M2o, `active` | The chart of accounts (GL accounts). `account_type` drives behavior (receivable/payable/income/expense/…). |
| `account.journal` | `account_journal` | `name` (req), `code` (req), `journal_type` Selection (sale/purchase/cash/bank/general), `company_id` M2o, `default_account_id` M2o→account.account, `sequence_code` Text, `active` | Where moves are booked; `code` drives numbering. `type` is a Rust keyword → field is `journal_type`. |
| `account.move` | `account_move` | `name` (def "/"), `move_type` Selection (entry/out_invoice/in_invoice/...), `date` Date, `journal_id` M2o (req), `partner_id` M2o, `state` Selection (draft/posted/cancel), `ref` Text, `line_ids` O2m, `amount_total`/`amount_untaxed`/`amount_tax` (aggregates), `currency_id` M2o | The journal entry / invoice document. Mailed (chatter audit). |
| `account.move.line` | `account_move_line` | `move_id` M2o (req), `account_id` M2o→account.account (req), `partner_id` M2o, `name` Text, `debit` Decimal, `credit` Decimal, `balance` (on-read = debit−credit), `tax_id` M2o→account.tax, `date` Date | The postings. Two Decimal columns (Odoo model); a line is debit XOR credit. |

`account_type` (v1 set): `receivable`, `payable`, `bank_cash`, `current_asset`, `fixed_asset`,
`current_liability`, `equity`, `income`, `expense`, `tax`. Enough for the invoicing path
(receivable + income + tax) and bills (payable + expense + tax).

## The balanced-entry invariant (M16.2 — the crux)

`register_constraint!("account.move", &["line_ids"], check_balanced)` — runs in-tx after the move +
its lines are written, sums the lines' `debit` and `credit` (via `ComputeInput` children), and returns
`Err` (→ rollback) unless `Σdebit == Σcredit`. This is the canonical `@api.constrains` use case that
single-row CHECK constraints cannot express — exactly what M11 was built for. Empty moves (no lines)
are allowed in draft; the balance check applies whenever there are lines.

Order totals are existing One2many aggregates over the lines (`amount_total` = Σ debit on the
non-tax / receivable side — for v1 `amount_total` = Σ `balance` of the partner/receivable line, or
simply Σ debit; refined in M16.4 where invoices have a known shape).

## Posting workflow (M16.3)

- `post` action: draft → posted; assigns `name` from the journal's sequence (`sequence_code`, e.g.
  `INV` → `INV/00001`), re-checks balance. Registered with `account.user`.
- `button_draft`: posted → draft (reset, for correction); `button_cancel`: → cancel.
- **Posted immutability**: a posted move's lines must not change. Enforced by an `@api.constrains` on
  `account.move` rejecting line writes while `state == 'posted'` (the constraint sees the post-write
  state; a write that leaves state posted AND changed lines is rejected). D6 can't express "lock when
  posted" (it's state-dependent), so the constraint is the lock — consistent with how M15.3 found D6
  unsuitable for state-conditional locks.

## Invoicing integration (M16.4)

Wire `sale.order.create_invoice` (and `purchase.order`) to generate a real `account.move`
(`out_invoice` / `in_invoice`) from the order lines: a receivable/payable line (partner, Σ total) plus
income/expense lines per order line plus tax lines, posted and balanced. Direction: `sales`/`purchase`
gain a dep on `account` (Odoo's direction). A minimal CoA + journals are **seeded at migrate** when
`account` is installed (analogous to base's currency/company seed) so the path works out of the box.

## Slices (each: implement → adversarial review → tests + live smoke → standalone commit + push)

1. **M16.1** — `account.account` + `account.journal` + ACLs (`account.user`/`account.manager`). Admin
   bootstrap now grants every registered group (no per-module edit). Tests + smoke.
2. **M16.2** — `account.move` + `account.move.line` + `balance` on-read + amount aggregates +
   `check_balanced` constraint. Tests: balanced move saves, unbalanced rolls back.
3. **M16.3** — `post`/`draft`/`cancel` actions + per-journal sequence + posted-move immutability.
4. **M16.4** — `create_invoice` → posted `account.move`; seed CoA + journals at migrate.

## Risks
- Unbalanced posting → `check_balanced` (in-tx, after children).
- Editing a posted move → posted-immutability constraint.
- Company consistency (a line's account must share the move's company) → constraint / company scope.
- Float vs exact money → `Decimal` end-to-end (debit/credit are `Decimal`, like the sale line money).
- Sequence per journal → reuse `ensure_sequence` keyed by the journal `sequence_code`.
