//! Account cross-record services — the invoicing / billing / payment / posting engine, relocated from
//! meshble-db onto the framework's `register_service!` seam so the ERP becomes an optional layer (the core
//! no longer names account.move / sale.order). The account module owns invoicing because it owns the GL;
//! it registers services on the ORDER models (sale.order / purchase.order) resolved at runtime, so no
//! cross-module crate dep.
//!
//! ATOMICITY (owner-approved): each service runs on the ONE transaction run_service opens — the order
//! claim (guarded_cas), the move + lines insert (insert_in_tx), the residual draw-down, and the posting
//! state flip (update_in_tx) commit together or roll back together. A crash or error anywhere leaves no
//! claimed-but-uninvoiced order and no drawn-down-but-unpaid invoice. The one deliberate exception is
//! sequence NUMBERING (ensure_sequence/next_value on the pool): a rollback after the number is claimed
//! gaps the sequence — the same pre-existing tradeoff every failure path already had.
//!
//! Reads of COMMITTED reference data (chart, journals, FX rates, company currency, fiscal lock, tax
//! buckets over order lines) stay on the pool ServiceCtx hands out; only state the service itself writes
//! in-tx (the move and its lines) must be read back through `cx.tx()`.

use meshble::prelude::*; // ServiceCtx, ServiceInput, ServiceOutput, DbError, Domain, Ctx
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::Row;

/// Posts an `account.move` (draft → posted): re-checks the balanced-entry invariant, enforces the fiscal
/// lock, numbers the entry from its journal's sequence, and flips state to posted. A shared helper the
/// invoice/bill/payment services call after building a draft move, and the body of the `post` service.
/// `ctx` is the authorization context (the caller for the `post` service; elevated when called by the
/// engine after creating a move). Returns the assigned entry number. Relocated from `Db::post_move`.
///
/// Runs on the SERVICE transaction: the move and its lines are read through `cx.tx()` (they may have been
/// inserted, uncommitted, earlier in this same tx by create_invoice/vendor_bill/register_payment), and the
/// state flip goes through `update_in_tx`, so create + post commit atomically. For the standalone `post`
/// service the target's visibility was already gated by run_service on this exact id; the journal read
/// stays SECURED under the caller (committed config — the caller's journal-visibility gate is preserved).
pub(crate) async fn post_move(cx: &mut ServiceCtx<'_, '_>, ctx: &Ctx, move_id: i64) -> Result<String, DbError> {
    let move_model = cx.resolve("account.move")?;
    let journal_model = cx.resolve("account.journal")?;

    // FOR UPDATE serializes concurrent posts of the SAME move from the very first read: the loser blocks
    // here until the winner commits, then re-reads state='posted' and fails below — before consuming a
    // sequence number or double-flipping the entry. (For an engine-created move this tx already owns the
    // row, so the lock is a no-op.)
    let (state, mdate, company_id, journal_id) = {
        let tx = cx.tx();
        let row = sqlx::query(
            "SELECT state, date::text AS date, company_id, journal_id FROM account_move WHERE id = $1 FOR UPDATE",
        )
        .bind(move_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| DbError::BadInput("move not found or not permitted".to_string()))?;
        (
            row.try_get::<Option<String>, _>("state")?.unwrap_or_default(),
            row.try_get::<Option<String>, _>("date")?,
            row.try_get::<Option<i64>, _>("company_id")?,
            row.try_get::<Option<i64>, _>("journal_id")?,
        )
    };
    if state != "draft" {
        return Err(DbError::BadInput(format!("only a draft entry can be posted (state is '{state}')")));
    }

    // Fiscal lock: an entry dated on or before its company's lock date cannot be posted (ISO dates compare
    // lexically; a move with no date/company, or a company with no lock, is free). Committed config → pool.
    if let (Some(md), Some(cid)) = (mdate.as_deref(), company_id) {
        if let Some(lock) = company_lock_date(cx, cid).await? {
            if md <= lock.as_str() {
                return Err(DbError::BadInput(format!(
                    "cannot post an entry dated {md}: on or before the fiscal lock date {lock}"
                )));
            }
        }
    }

    // Re-check the balance at post time (defense in depth — create already enforced it). Summed on the tx
    // so just-inserted lines are visible; the invariant is a property of the WHOLE entry, so it sums every
    // line (a visibility-filtered read could false-fail a caller whose record rules hide some lines).
    let (debit, credit, nlines) = {
        let tx = cx.tx();
        let row = sqlx::query(
            "SELECT COALESCE(SUM(debit), 0) AS d, COALESCE(SUM(credit), 0) AS c, COUNT(*) AS n \
             FROM account_move_line WHERE move_id = $1",
        )
        .bind(move_id)
        .fetch_one(&mut **tx)
        .await?;
        (row.try_get::<Decimal, _>("d")?, row.try_get::<Decimal, _>("c")?, row.try_get::<i64, _>("n")?)
    };
    if nlines == 0 {
        return Err(DbError::BadInput("cannot post an entry with no lines".to_string()));
    }
    if debit != credit {
        return Err(DbError::BadInput(format!("cannot post an unbalanced entry: debit {debit} != credit {credit}")));
    }

    // Number the entry from its journal's sequence (sequence_code, else the journal code).
    let journal_id = journal_id.ok_or_else(|| DbError::BadInput("the move has no journal".to_string()))?;
    let journal = cx
        .find_one_secured(&journal_model, ctx, journal_id)
        .await?
        .ok_or_else(|| DbError::BadInput("journal not found or not permitted".to_string()))?;
    let sc = journal.get("sequence_code").and_then(|v| v.as_str()).unwrap_or("");
    let code = journal.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let seq = if !sc.is_empty() {
        sc
    } else if !code.is_empty() {
        code
    } else {
        return Err(DbError::BadInput("the journal has no sequence code".to_string()));
    };
    // Pool-side numbering: a rollback below gaps the sequence (pre-existing tradeoff, see module header).
    cx.ensure_sequence(seq, &format!("{seq}/"), "", 5).await?;
    let number = cx.next_value(seq).await?;

    let payload = json!({ "state": "posted", "name": number });
    if cx.update_in_tx(&move_model, ctx, move_id, payload.as_object().unwrap()).await? == 0 {
        // A write-rule-filtered row would previously "post" silently (number returned, state unchanged);
        // atomically, a no-op flip is an error so the whole service rolls back instead of lying.
        return Err(DbError::BadInput("move not found or not permitted".to_string()));
    }
    Ok(number)
}

/// The `post` service on account.move: POST /api/account.move/:id/service/post. run_service has gated
/// Write + visibility on the move; this runs post_move under the caller and returns the entry number.
pub async fn post(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let ctx = cx.caller().clone();
    let number = post_move(cx, &ctx, input.record_id).await?;
    Ok(ServiceOutput::json(json!({ "posted": number })))
}

/// Generates a posted customer invoice from a confirmed sale order (POST
/// /api/sale.order/:id/service/create_invoice): a balanced out_invoice (income credit + per-group tax
/// credits + receivable debit, in company currency), then claims the order (to_invoice → invoiced) and
/// posts. run_service gated Write + visibility on the order. Relocated from `Db::create_sale_invoice`.
pub async fn create_invoice(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let order_model = cx.resolve("sale.order")?;
    let account_model = cx
        .resolve("account.account")
        .map_err(|_| DbError::BadInput("install the account module to invoice".to_string()))?;
    let move_model = cx.resolve("account.move")?;
    let ctx = cx.caller().clone();
    let order_id = input.record_id;

    let order = cx
        .find_one_secured(&order_model, &ctx, order_id)
        .await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
    let status = order.get("invoice_status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "to_invoice" {
        return Err(DbError::BadInput(format!("order is not ready to invoice (invoice status '{status}')")));
    }
    let partner = order.get("partner_id").and_then(|v| v.as_i64());
    let currency = order.get("currency_id").and_then(|v| v.as_i64());
    let company = order.get("company_id").and_then(|v| v.as_i64()).or(ctx.company_id);
    let amount = |k: &str| -> Decimal {
        order.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
    };
    let (untaxed, tax, total) = (amount("amount_untaxed"), amount("amount_tax"), amount("amount_total"));
    if total <= Decimal::ZERO {
        return Err(DbError::BadInput("cannot invoice an order with a non-positive total".to_string()));
    }

    // Resolve the chart BEFORE claiming the order, so a misconfiguration fails before any side effect.
    let elevated = cx.elevated();
    let receivable = cx
        .first_match(&account_model, "account_type", "receivable", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no receivable account configured".to_string()))?;
    let income = cx
        .first_match(&account_model, "account_type", "income", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no income account configured".to_string()))?;
    let journal = cx
        .first_match(&cx.resolve("account.journal")?, "journal_type", "sale", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no sale journal configured".to_string()))?;
    let tax_account = if tax != Decimal::ZERO {
        Some(
            cx.first_match(&account_model, "account_type", "tax", company)
                .await?
                .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
        )
    } else {
        None
    };

    // CLAIM the order atomically (compare-and-set to_invoice -> invoiced) under the caller's row-level
    // authorization; abort (no GL effect) if already claimed or not permitted.
    if !cx.guarded_cas(&order_model, order_id, "invoice_status = 'invoiced'", "invoice_status = 'to_invoice'").await? {
        return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_invoice" });
    }

    // Accounting date today; due date today + the payment term (order's, else the customer's default).
    let today = cx.today().await?;
    let term_id = match order.get("payment_term_id").and_then(|v| v.as_i64()) {
        Some(t) => Some(t),
        None => match partner {
            Some(p) => sqlx::query_scalar::<_, Option<i64>>("SELECT NULLIF(property_payment_term_id, 0) FROM res_partner WHERE id = $1")
                .bind(p)
                .fetch_optional(cx.pool())
                .await?
                .flatten(),
            None => None,
        },
    };
    let due_date = match term_id {
        Some(tid) => sqlx::query_scalar::<_, Option<String>>(
            "SELECT ($1::date + days::int)::text FROM account_payment_term WHERE id = $2 AND active",
        )
        .bind(&today)
        .bind(tid)
        .fetch_optional(cx.pool())
        .await?
        .flatten()
        .unwrap_or_else(|| today.clone()),
        None => today.clone(),
    };
    // Company currency once; FX applies only when it differs from the invoice currency.
    let co_cur: Option<i64> = match company {
        Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
            .bind(co)
            .fetch_optional(cx.pool())
            .await?
            .flatten(),
        None => None,
    };
    let fx = match (currency, co_cur) {
        (Some(c), Some(cc)) if c != cc => Some((c, cc)),
        _ => None,
    };
    let untaxed_co = match fx {
        Some((c, cc)) => convert_amount(cx.pool(), untaxed, c, cc, &today).await?,
        None => untaxed,
    };

    let buckets = tax_group_buckets(cx, order_id, "sale_order_line_tax", "sale_order_line", tax).await?;
    let tax_account = match tax_account {
        Some(a) => Some(a),
        None if !buckets.is_empty() => Some(
            cx.first_match(&account_model, "account_type", "tax", company)
                .await?
                .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
        ),
        None => None,
    };

    // Balanced invoice: income credit (untaxed) + one tax credit per group + receivable debit, all in the
    // company currency. amount_currency carries the signed invoice-currency amount (+ debit, − credit). The
    // receivable is the SUM of the already-rounded company-currency parts, never an independent convert.
    let mut lines = vec![json!({
        "account_id": income, "name": "Untaxed Amount", "debit": "0", "credit": untaxed_co.to_string(),
        "amount_currency": (-untaxed).to_string(), "partner_id": partner, "company_id": company
    })];
    let mut tax_co_total = Decimal::ZERO;
    for (name, amt) in &buckets {
        let amt_co = match fx {
            Some((c, cc)) => convert_amount(cx.pool(), *amt, c, cc, &today).await?,
            None => *amt,
        };
        tax_co_total += amt_co;
        lines.push(json!({
            "account_id": tax_account, "name": name, "debit": "0", "credit": amt_co.to_string(),
            "amount_currency": (-*amt).to_string(), "partner_id": partner, "company_id": company
        }));
    }
    let receivable_co = untaxed_co + tax_co_total;
    lines.push(json!({
        "account_id": receivable, "name": "Receivable", "debit": receivable_co.to_string(), "credit": "0",
        "amount_currency": total.to_string(), "partner_id": partner, "company_id": company
    }));

    let move_payload = json!({
        "move_type": "out_invoice", "journal_id": journal, "partner_id": partner,
        "currency_id": currency, "company_id": company, "line_ids": lines,
        "date": today, "invoice_date_due": due_date,
        "amount_residual": total.to_string(), "amount_total_company": receivable_co.to_string()
    });
    // In-tx: the move + lines commit with the claim above; a posting failure rolls the claim back too.
    let move_id = cx.insert_in_tx(&move_model, &elevated, move_payload.as_object().unwrap()).await?;
    post_move(cx, &elevated, move_id).await?;
    Ok(ServiceOutput::json(json!({ "invoice": move_id })))
}

/// Generates a posted vendor bill from a confirmed purchase order (POST
/// /api/purchase.order/:id/service/create_vendor_bill): the buy-side mirror of create_invoice (expense
/// debit + per-group tax debits + payable credit), then claims + posts. Relocated from
/// `Db::create_vendor_bill`.
pub async fn create_vendor_bill(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let order_model = cx.resolve("purchase.order")?;
    let account_model = cx
        .resolve("account.account")
        .map_err(|_| DbError::BadInput("install the account module to bill".to_string()))?;
    let move_model = cx.resolve("account.move")?;
    let ctx = cx.caller().clone();
    let order_id = input.record_id;

    let order = cx
        .find_one_secured(&order_model, &ctx, order_id)
        .await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
    let status = order.get("invoice_status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "to_invoice" {
        return Err(DbError::BadInput(format!("order is not ready to bill (billing status '{status}')")));
    }
    let partner = order.get("partner_id").and_then(|v| v.as_i64());
    let currency = order.get("currency_id").and_then(|v| v.as_i64());
    let company = order.get("company_id").and_then(|v| v.as_i64()).or(ctx.company_id);
    let amount = |k: &str| -> Decimal {
        order.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
    };
    let (untaxed, tax, total) = (amount("amount_untaxed"), amount("amount_tax"), amount("amount_total"));
    if total <= Decimal::ZERO {
        return Err(DbError::BadInput("cannot bill an order with a non-positive total".to_string()));
    }

    let elevated = cx.elevated();
    let payable = cx
        .first_match(&account_model, "account_type", "payable", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no payable account configured".to_string()))?;
    let expense = cx
        .first_match(&account_model, "account_type", "expense", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no expense account configured".to_string()))?;
    let journal = cx
        .first_match(&cx.resolve("account.journal")?, "journal_type", "purchase", company)
        .await?
        .ok_or_else(|| DbError::BadInput("no purchase journal configured".to_string()))?;
    let tax_account = if tax != Decimal::ZERO {
        Some(
            cx.first_match(&account_model, "account_type", "tax", company)
                .await?
                .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
        )
    } else {
        None
    };

    if !cx.guarded_cas(&order_model, order_id, "invoice_status = 'invoiced'", "invoice_status = 'to_invoice'").await? {
        return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_vendor_bill" });
    }

    let today = cx.today().await?;
    let co_cur: Option<i64> = match company {
        Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
            .bind(co)
            .fetch_optional(cx.pool())
            .await?
            .flatten(),
        None => None,
    };
    let fx = match (currency, co_cur) {
        (Some(c), Some(cc)) if c != cc => Some((c, cc)),
        _ => None,
    };
    let untaxed_co = match fx {
        Some((c, cc)) => convert_amount(cx.pool(), untaxed, c, cc, &today).await?,
        None => untaxed,
    };
    let buckets = tax_group_buckets(cx, order_id, "purchase_order_line_tax", "purchase_order_line", tax).await?;
    let tax_account = match tax_account {
        Some(a) => Some(a),
        None if !buckets.is_empty() => Some(
            cx.first_match(&account_model, "account_type", "tax", company)
                .await?
                .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
        ),
        None => None,
    };

    let mut lines = vec![json!({
        "account_id": expense, "name": "Untaxed Amount", "debit": untaxed_co.to_string(), "credit": "0",
        "amount_currency": untaxed.to_string(), "partner_id": partner, "company_id": company
    })];
    let mut tax_co_total = Decimal::ZERO;
    for (name, amt) in &buckets {
        let amt_co = match fx {
            Some((c, cc)) => convert_amount(cx.pool(), *amt, c, cc, &today).await?,
            None => *amt,
        };
        tax_co_total += amt_co;
        lines.push(json!({
            "account_id": tax_account, "name": name, "debit": amt_co.to_string(), "credit": "0",
            "amount_currency": amt.to_string(), "partner_id": partner, "company_id": company
        }));
    }
    let payable_co = untaxed_co + tax_co_total;
    lines.push(json!({
        "account_id": payable, "name": "Payable", "debit": "0", "credit": payable_co.to_string(),
        "amount_currency": (-total).to_string(), "partner_id": partner, "company_id": company
    }));

    let move_payload = json!({
        "move_type": "in_invoice", "journal_id": journal, "partner_id": partner,
        "currency_id": currency, "company_id": company, "line_ids": lines,
        "date": today, "invoice_date_due": today,
        "amount_residual": total.to_string(), "amount_total_company": payable_co.to_string()
    });
    // In-tx: the bill + lines commit with the claim above; a posting failure rolls the claim back too.
    let move_id = cx.insert_in_tx(&move_model, &elevated, move_payload.as_object().unwrap()).await?;
    post_move(cx, &elevated, move_id).await?;
    Ok(ServiceOutput::json(json!({ "bill": move_id })))
}

/// Registers a (full or partial) payment against a posted customer invoice / vendor bill (POST
/// /api/account.move/:id/service/register_payment, body {amount, journal_id}): atomically draws down the
/// open residual, then books a balanced payment entry (with a realized-FX plug line when the company and
/// invoice currencies differ), posted. Relocated from `Db::register_payment`.
pub async fn register_payment(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let move_model = cx.resolve("account.move")?;
    let journal_model = cx.resolve("account.journal")?;
    let account_model = cx.resolve("account.account")?;
    let ctx = cx.caller().clone();
    let invoice_id = input.record_id;
    // Accept `amount` as a JSON string ("100") OR a JSON number (100) — the pre-relocation handler took
    // both, so keep the contract wide.
    let amount: Decimal = match input.body.get("amount") {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").parse(),
        Some(v) if v.is_number() => v.to_string().parse(),
        _ => return Err(DbError::BadInput("'amount' is required".to_string())),
    }
    .map_err(|_| DbError::BadInput("payment amount must be a number".to_string()))?;
    // Fail fast at the boundary (the old handler 400'd on a missing/non-integer journal_id rather than
    // letting it coerce to 0 and surface as a late "journal not found").
    let journal_id = input
        .body
        .get("journal_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| DbError::BadInput("'journal_id' is required".to_string()))?;

    if amount <= Decimal::ZERO {
        return Err(DbError::BadInput("payment amount must be positive".to_string()));
    }
    let inv = cx
        .find_one_secured(&move_model, &ctx, invoice_id)
        .await?
        .ok_or_else(|| DbError::BadInput("invoice not found or not permitted".to_string()))?;
    if inv.get("state").and_then(|v| v.as_str()) != Some("posted") {
        return Err(DbError::BadInput("only a posted invoice can be paid".to_string()));
    }
    let is_customer = match inv.get("move_type").and_then(|v| v.as_str()).unwrap_or("") {
        "out_invoice" => true,
        "in_invoice" => false,
        _ => return Err(DbError::BadInput("payments apply to customer invoices or vendor bills".to_string())),
    };
    let partner = inv.get("partner_id").and_then(|v| v.as_i64());
    let currency = inv.get("currency_id").and_then(|v| v.as_i64());
    let company = inv.get("company_id").and_then(|v| v.as_i64());

    let elevated = cx.elevated();
    let journal = cx
        .find_one_secured(&journal_model, &elevated, journal_id)
        .await?
        .ok_or_else(|| DbError::BadInput("journal not found".to_string()))?;
    match journal.get("journal_type").and_then(|v| v.as_str()).unwrap_or("") {
        "bank" | "cash" => {}
        _ => return Err(DbError::BadInput("a payment needs a bank or cash journal".to_string())),
    }
    let money = journal
        .get("default_account_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| DbError::BadInput("the payment journal has no default account".to_string()))?;
    let counter_type = if is_customer { "receivable" } else { "payable" };
    let counterpart = cx
        .first_match(&account_model, "account_type", counter_type, company)
        .await?
        .ok_or_else(|| DbError::BadInput(format!("no {counter_type} account configured")))?;

    // Atomically draw down the open residual (validates no over-payment + records settlement state). The
    // CASE reads the OLD residual (Postgres evaluates SET RHS against the pre-update row). On the SERVICE
    // tx: the draw-down commits with the payment entry below, or rolls back with it — a failure while
    // booking the entry can no longer leave the invoice drawn down without a payment on the books.
    let row = {
        let tx = cx.tx();
        sqlx::query(
            "UPDATE account_move \
             SET amount_residual = amount_residual - $2, \
                 payment_state = CASE WHEN amount_residual - $2 <= 0 THEN 'paid' ELSE 'partial' END, \
                 reconciled = (amount_residual - $2 <= 0) \
             WHERE id = $1 AND amount_residual >= $2 \
             RETURNING id",
        )
        .bind(invoice_id)
        .bind(amount)
        .fetch_optional(&mut **tx)
        .await?
    };
    if row.is_none() {
        return Err(DbError::BadInput("payment exceeds the invoice's open balance".to_string()));
    }

    // Balanced payment entry. Multi-currency: bank at TODAY's rate, counterpart relieved at the invoice
    // date rate; the difference is the realized FX gain/loss on a 3rd line so the company-currency entry
    // balances. Same/absent company currency ⇒ both equal `amount`, no FX line.
    let today = cx.today().await?;
    let invoice_date = inv.get("date").and_then(|v| v.as_str()).unwrap_or(today.as_str()).to_string();
    let co_cur: Option<i64> = match company {
        Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
            .bind(co)
            .fetch_optional(cx.pool())
            .await?
            .flatten(),
        None => None,
    };
    let fx = match (currency, co_cur) {
        (Some(c), Some(cc)) if c != cc => Some((c, cc)),
        _ => None,
    };
    let (money_company, counter_company) = match fx {
        Some((c, cc)) => (
            convert_amount(cx.pool(), amount, c, cc, &today).await?,
            convert_amount(cx.pool(), amount, c, cc, &invoice_date).await?,
        ),
        None => (amount, amount),
    };

    let (bank_d, bank_c, bank_cur, ctr_d, ctr_c, ctr_cur) = if is_customer {
        (money_company, Decimal::ZERO, amount, Decimal::ZERO, counter_company, -amount)
    } else {
        (Decimal::ZERO, money_company, -amount, counter_company, Decimal::ZERO, amount)
    };
    let mut lines = vec![
        json!({ "account_id": money, "name": "Payment", "debit": bank_d.to_string(), "credit": bank_c.to_string(), "amount_currency": bank_cur.to_string(), "partner_id": partner, "company_id": company }),
        json!({ "account_id": counterpart, "name": "Payment", "debit": ctr_d.to_string(), "credit": ctr_c.to_string(), "amount_currency": ctr_cur.to_string(), "partner_id": partner, "company_id": company }),
    ];
    let imbalance = (bank_d + ctr_d) - (bank_c + ctr_c);
    if imbalance != Decimal::ZERO {
        let (fx_d, fx_c, gl_type) = if imbalance > Decimal::ZERO {
            (Decimal::ZERO, imbalance, "income")
        } else {
            (-imbalance, Decimal::ZERO, "expense")
        };
        let fx_account = cx
            .first_match(&account_model, "account_type", gl_type, company)
            .await?
            .ok_or_else(|| DbError::BadInput(format!("no {gl_type} account configured for FX gain/loss")))?;
        lines.push(json!({ "account_id": fx_account, "name": "Exchange difference", "debit": fx_d.to_string(), "credit": fx_c.to_string(), "amount_currency": "0", "partner_id": partner, "company_id": company }));
    }
    let lines = serde_json::Value::Array(lines);
    let pay_payload = json!({
        "move_type": "entry", "journal_id": journal_id, "partner_id": partner,
        "currency_id": currency, "company_id": company, "line_ids": lines
    });
    // In-tx: the payment entry commits with the residual draw-down above, and posting with both.
    let pay_id = cx.insert_in_tx(&move_model, &elevated, pay_payload.as_object().unwrap()).await?;
    post_move(cx, &elevated, pay_id).await?;
    Ok(ServiceOutput::json(json!({ "payment": pay_id })))
}

// ── module-owned bespoke SQL helpers (relocated from Db), run on the pool ServiceCtx hands out ──

// ── read-only REPORTS (relocated from Db), dispatched by GET /api/reports/<name> via Db::run_report,
//    which gates Read on the declared model before calling these. Pool-based aggregate queries. ──

/// Trial balance: per-account totals (debit, credit, balance) over POSTED entries, accounts with activity
/// only, ordered by code. Relocated from Db::trial_balance (the Read-on-account.account gate is now in
/// run_report). v1: all companies.
pub async fn trial_balance(pool: &sqlx::PgPool, _params: serde_json::Map<String, serde_json::Value>) -> Result<Vec<serde_json::Value>, DbError> {
    let rows = sqlx::query(
        "SELECT a.id, a.code, a.name, a.account_type, \
                COALESCE(SUM(l.debit), 0) AS debit, COALESCE(SUM(l.credit), 0) AS credit \
         FROM account_account a \
         JOIN account_move_line l ON l.account_id = a.id \
         JOIN account_move m ON m.id = l.move_id AND m.state = 'posted' \
         GROUP BY a.id, a.code, a.name, a.account_type \
         ORDER BY a.code",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let debit: Decimal = r.try_get("debit").unwrap_or_default();
            let credit: Decimal = r.try_get("credit").unwrap_or_default();
            json!({
                "account_id": r.try_get::<i64, _>("id").unwrap_or_default(),
                "code": r.try_get::<Option<String>, _>("code").ok().flatten(),
                "name": r.try_get::<Option<String>, _>("name").ok().flatten(),
                "account_type": r.try_get::<Option<String>, _>("account_type").ok().flatten(),
                "debit": debit.to_string(),
                "credit": credit.to_string(),
                "balance": (debit - credit).to_string(),
            })
        })
        .collect())
}

/// Aged receivable/payable: open posted invoices bucketed by days past due, grouped by partner. Query
/// param `kind` = receivable (out_invoice) / payable (in_invoice). Relocated from Db::aged_balance.
pub async fn aged_balance(pool: &sqlx::PgPool, params: serde_json::Map<String, serde_json::Value>) -> Result<Vec<serde_json::Value>, DbError> {
    let move_type = match params.get("kind").and_then(|v| v.as_str()).unwrap_or("") {
        "receivable" => "out_invoice",
        "payable" => "in_invoice",
        _ => return Err(DbError::BadInput("'kind' must be 'receivable' or 'payable'".to_string())),
    };
    let rows = sqlx::query(
        "SELECT m.partner_id, p.name AS partner_name, \
                COALESCE(SUM(m.amount_residual) FILTER (WHERE m.invoice_date_due IS NULL OR m.invoice_date_due >= current_date), 0) AS bucket_current, \
                COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 1 AND 30), 0) AS b1_30, \
                COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 31 AND 60), 0) AS b31_60, \
                COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 61 AND 90), 0) AS b61_90, \
                COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due > 90), 0) AS b90_plus, \
                COALESCE(SUM(m.amount_residual), 0) AS total \
         FROM account_move m \
         LEFT JOIN res_partner p ON p.id = m.partner_id \
         WHERE m.state = 'posted' AND m.move_type = $1 AND m.amount_residual > 0 \
         GROUP BY m.partner_id, p.name \
         ORDER BY p.name",
    )
    .bind(move_type)
    .fetch_all(pool)
    .await?;
    let d = |r: &sqlx::postgres::PgRow, c: &str| -> String {
        r.try_get::<Decimal, _>(c).unwrap_or_default().to_string()
    };
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "partner_id": r.try_get::<Option<i64>, _>("partner_id").ok().flatten(),
                "partner_name": r.try_get::<Option<String>, _>("partner_name").ok().flatten(),
                "current": d(r, "bucket_current"),
                "b1_30": d(r, "b1_30"),
                "b31_60": d(r, "b31_60"),
                "b61_90": d(r, "b61_90"),
                "b90_plus": d(r, "b90_plus"),
                "total": d(r, "total"),
            })
        })
        .collect())
}

/// General-ledger drill-down: every posted move line on one account in date order with a running balance.
/// Query param `account_id`. Relocated from Db::general_ledger.
pub async fn general_ledger(pool: &sqlx::PgPool, params: serde_json::Map<String, serde_json::Value>) -> Result<Vec<serde_json::Value>, DbError> {
    let account_id: i64 = params
        .get("account_id")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .ok_or_else(|| DbError::BadInput("'account_id' is required".to_string()))?;
    let rows = sqlx::query(
        "SELECT m.date::text AS date, m.name AS move_name, m.move_type, l.name AS label, \
                l.partner_id, p.name AS partner_name, l.debit, l.credit \
         FROM account_move_line l \
         JOIN account_move m ON m.id = l.move_id AND m.state = 'posted' \
         LEFT JOIN res_partner p ON p.id = l.partner_id \
         WHERE l.account_id = $1 \
         ORDER BY m.date, m.id, l.id",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    let mut running = Decimal::ZERO;
    Ok(rows
        .iter()
        .map(|r| {
            let debit: Decimal = r.try_get("debit").unwrap_or_default();
            let credit: Decimal = r.try_get("credit").unwrap_or_default();
            running += debit - credit;
            json!({
                "date": r.try_get::<Option<String>, _>("date").ok().flatten(),
                "move_name": r.try_get::<Option<String>, _>("move_name").ok().flatten(),
                "move_type": r.try_get::<Option<String>, _>("move_type").ok().flatten(),
                "label": r.try_get::<Option<String>, _>("label").ok().flatten(),
                "partner_id": r.try_get::<Option<i64>, _>("partner_id").ok().flatten(),
                "partner_name": r.try_get::<Option<String>, _>("partner_name").ok().flatten(),
                "debit": debit.to_string(),
                "credit": credit.to_string(),
                "balance": running.to_string(),
            })
        })
        .collect())
}

/// The exchange rate for `currency` on or before `as_of` (currency units per 1 base unit): the latest
/// res.currency.rate row. No rows = base currency (1.0); rows but none on/before `as_of` = error.
async fn currency_rate(pool: &sqlx::PgPool, currency: i64, as_of: &str) -> Result<Decimal, DbError> {
    let latest: Option<Decimal> = sqlx::query_scalar(
        "SELECT rate FROM res_currency_rate WHERE currency_id = $1 AND name <= $2::date ORDER BY name DESC LIMIT 1",
    )
    .bind(currency)
    .bind(as_of)
    .fetch_optional(pool)
    .await?;
    if let Some(r) = latest {
        return Ok(r);
    }
    let has_any: Option<i64> = sqlx::query_scalar("SELECT 1 FROM res_currency_rate WHERE currency_id = $1 LIMIT 1")
        .bind(currency)
        .fetch_optional(pool)
        .await?;
    if has_any.is_some() {
        Err(DbError::BadInput(format!("no exchange rate for currency {currency} on or before {as_of}")))
    } else {
        Ok(Decimal::ONE)
    }
}

/// Converts `amount` from `from_currency` to `to_currency` at the rates effective on `as_of`, rounded to
/// the to-currency's decimal places. Two-hop through the base currency (Odoo's `_convert`).
pub async fn convert_amount(pool: &sqlx::PgPool, amount: Decimal, from_currency: i64, to_currency: i64, as_of: &str) -> Result<Decimal, DbError> {
    if from_currency == to_currency {
        return Ok(amount);
    }
    let from_rate = currency_rate(pool, from_currency, as_of).await?;
    let to_rate = currency_rate(pool, to_currency, as_of).await?;
    if from_rate.is_zero() {
        return Err(DbError::BadInput("source currency rate is zero".to_string()));
    }
    let dp: Option<i64> = sqlx::query_scalar::<_, Option<i64>>("SELECT decimal_places FROM res_currency WHERE id = $1")
        .bind(to_currency)
        .fetch_optional(pool)
        .await?
        .flatten();
    Ok((amount * to_rate / from_rate).round_dp(dp.unwrap_or(2).max(0) as u32))
}

/// The company's fiscal lock date as an ISO YYYY-MM-DD string, or None.
async fn company_lock_date(cx: &ServiceCtx<'_, '_>, company_id: i64) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar::<_, Option<String>>("SELECT fiscalyear_lock_date::text FROM res_company WHERE id = $1")
        .bind(company_id)
        .fetch_optional(cx.pool())
        .await?
        .flatten())
}

/// Per-group tax totals (order currency) from a line's materialized breakdown, plus a single fallback
/// bucket for any tax NOT in the breakdown, so the GL tax always sums to the order's amount_tax. Ordered by
/// group sequence (NULL group last). Relocated from `Db::tax_group_buckets`.
async fn tax_group_buckets(
    cx: &ServiceCtx<'_, '_>,
    order_id: i64,
    breakdown_table: &str,
    line_table: &str,
    total_tax: Decimal,
) -> Result<Vec<(String, Decimal)>, DbError> {
    let sql = format!(
        "SELECT t.tax_group_id, g.name AS gname, SUM(t.tax_amount) AS amt \
         FROM {breakdown_table} t JOIN {line_table} l ON l.id = t.line_id \
         LEFT JOIN account_tax_group g ON g.id = t.tax_group_id \
         WHERE l.order_id = $1 \
         GROUP BY t.tax_group_id, g.name, g.sequence \
         ORDER BY COALESCE(g.sequence, 1000), t.tax_group_id"
    );
    let rows = sqlx::query(&sql).bind(order_id).fetch_all(cx.pool()).await?;
    let mut buckets: Vec<(String, Decimal)> = Vec::new();
    let mut breakdown_total = Decimal::ZERO;
    for r in &rows {
        let amt: Decimal = r.try_get::<Option<Decimal>, _>("amt")?.unwrap_or_default();
        breakdown_total += amt;
        if amt != Decimal::ZERO {
            let name = r.try_get::<Option<String>, _>("gname").ok().flatten().unwrap_or_else(|| "Taxes".to_string());
            buckets.push((name, amt));
        }
    }
    let fallback = total_tax - breakdown_total;
    if fallback != Decimal::ZERO {
        buckets.push(("Taxes".to_string(), fallback));
    }
    Ok(buckets)
}
