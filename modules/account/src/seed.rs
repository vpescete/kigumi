//! Chart-of-accounts seeding, run by the `register_seed!` seam at migrate while `account` is
//! installed: a minimal chart + the four standard journals for the default company, so a fresh
//! instance can invoice immediately. Skipped once any account exists (an operator's chart is never
//! touched). Relocated verbatim from kigumi-cli so adopter binaries get it too.

use kigumi::prelude::*;

fn res(name: &str) -> Result<ResolvedModel, DbError> {
    resolve_registered(name).map_err(|e| DbError::Migration(format!("{name} not resolvable: {e:?}")))
}

/// Seeds a minimal chart of accounts + journals for the default company.
pub async fn seed_account_data(db: &Db) -> Result<(), DbError> {
    let account = res("account.account")?;
    let journal = res("account.journal")?;
    let company = res("res.company")?;
    let su = Ctx::new(0, vec![]).sudo();

    if db.count_secured(&account, &su, &[], &[], None).await? > 0 {
        return Ok(()); // already has a chart
    }
    let Some(&comp_id) = db.find_ids_secured(&company, &su, &[], &[], None).await?.first() else {
        return Ok(()); // no company to scope the chart to yet
    };

    let acc = |code: &str, name: &str, atype: &str, reconcile: bool| {
        serde_json::json!({ "code": code, "name": name, "account_type": atype, "reconcile": reconcile, "company_id": comp_id, "active": true })
    };
    let new_account = |v: serde_json::Value| {
        let account = &account;
        let su = &su;
        async move { db.insert_secured(account, su, &[], &[], v.as_object().unwrap()).await }
    };
    let income = new_account(acc("400000", "Product Sales", "income", false)).await?;
    let expense = new_account(acc("600000", "Expenses", "expense", false)).await?;
    let bank = new_account(acc("101000", "Bank", "bank_cash", false)).await?;
    new_account(acc("121000", "Account Receivable", "receivable", true)).await?;
    new_account(acc("211000", "Account Payable", "payable", true)).await?;
    new_account(acc("251000", "Tax Received", "tax", false)).await?;

    let new_journal = |v: serde_json::Value| {
        let journal = &journal;
        let su = &su;
        async move { db.insert_secured(journal, su, &[], &[], v.as_object().unwrap()).await }
    };
    new_journal(serde_json::json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV", "default_account_id": income, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Vendor Bills", "code": "BILL", "journal_type": "purchase", "sequence_code": "BILL", "default_account_id": expense, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Bank", "code": "BNK", "journal_type": "bank", "sequence_code": "BNK", "default_account_id": bank, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Miscellaneous", "code": "MISC", "journal_type": "general", "sequence_code": "MISC", "company_id": comp_id, "active": true })).await?;
    println!("seeded chart of accounts + journals");
    Ok(())
}
