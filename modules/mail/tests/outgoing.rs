//! Outgoing mail queue: flush_outgoing_mail hands each `outgoing` mail.mail to the host's transport and
//! marks it sent / exception. The transport is a stub here (a real SMTP send is the app's job). Requires
//! DATABASE_URL.

use meshble::prelude::*;
use meshble_db::{Db, OutgoingMail};
use serde_json::json;
use std::sync::Mutex;

fn link() {
    let _ = (&meshble_mod_mail::MANIFEST, &meshble_mod_base::MANIFEST);
}

#[tokio::test]
async fn flush_sends_queued_mail_and_records_outcomes() {
    link();
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_m2m_relations(&t.model).await.unwrap(); }

    let mail = resolve_registered("mail.mail").unwrap();
    let m1 = db.insert_secured(&mail, &su, &[], &[], json!({
        "email_to": "ada@example.com", "email_from": "billing@acme.test", "subject": "Invoice INV/001",
        "body_html": "<p>Please find your invoice.</p>", "state": "outgoing"
    }).as_object().unwrap()).await.unwrap();

    // A recording stub transport: capture each mail, succeed.
    let sent_box: Mutex<Vec<OutgoingMail>> = Mutex::new(Vec::new());
    let ok_send = |m: &OutgoingMail| -> Result<(), String> {
        sent_box.lock().unwrap().push(m.clone());
        Ok(())
    };
    let n = db.flush_outgoing_mail(&ok_send).await.unwrap();
    assert_eq!(n, 1, "one mail sent");
    let captured = sent_box.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].to, "ada@example.com");
    assert_eq!(captured[0].subject, "Invoice INV/001");
    drop(captured);

    let after = db.find_one_secured(&mail, &su, &[], &[], m1).await.unwrap().unwrap();
    assert_eq!(after["state"], "sent", "the mail is marked sent");

    // Re-flushing sends nothing (no outgoing left).
    assert_eq!(db.flush_outgoing_mail(&ok_send).await.unwrap(), 0, "nothing left to send");

    // A failing transport marks the mail as an exception and records the error.
    let m2 = db.insert_secured(&mail, &su, &[], &[], json!({
        "email_to": "bad@", "subject": "Boom", "state": "outgoing"
    }).as_object().unwrap()).await.unwrap();
    let fail_send = |_: &OutgoingMail| -> Result<(), String> { Err("smtp 550 rejected".to_string()) };
    assert_eq!(db.flush_outgoing_mail(&fail_send).await.unwrap(), 0, "the failing send counts as not-sent");
    let failed = db.find_one_secured(&mail, &su, &[], &[], m2).await.unwrap().unwrap();
    assert_eq!(failed["state"], "exception");
    assert_eq!(failed["error_message"], "smtp 550 rejected", "the error is recorded");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
