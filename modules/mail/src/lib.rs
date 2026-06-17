//! Mail module: a headless chatter subsystem. A model opts in with one line
//! (`meshble::register_mailed!("sale.order")`) — no 5000-line mixin — and gains a thread of
//! `mail.message`s addressed by the polymorphic `(res_model, res_id)` link. The framework cleans
//! that thread up on delete (the integrity guarantee Odoo leaves to hand-written `unlink` overrides),
//! reliably, because Meshble has a single controlled delete path. See docs/MAIL_DESIGN.md.
//!
//! Slice 1 (this file): the `mail.message` model + post/list API (server) + delete cleanup (db).
//! Tracking, activities and followers land in later slices on the same `(res_model, res_id)` link.

use meshble::prelude::*;

/// Module manifest. `mail` depends on `base` (res.users as message author).
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "mail",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^1.0" }],
    summary: "Headless chatter: messages, tracking, followers, activities",
};
meshble::register_module!(MANIFEST);

/// A thread message: a human comment or a system audit entry, attached to any record via the
/// polymorphic `(res_model, res_id)` link. One shared table serves every mailed model — no
/// per-model message table. Append-only (no write/delete ACL for users); ordered by `id` (a
/// monotonic bigserial), so `date` is for display only and the thread reads chronologically by id.
// ponytail: no (res_model, res_id) index yet — the metamodel has no index DDL. Add when thread
// volume makes the seq-scan measurable; the lookup is res_model=? AND res_id=?.
#[model(name = "mail.message", table = "mail_message")]
pub struct MailMessage {
    /// Model name of the host record (e.g. "sale.order"). Validated against the registry by the API.
    #[field(label = "Document Model", required)]
    res_model: Text,

    /// Id of the host record within `res_model`. No FK (polymorphic); integrity via delete cleanup.
    #[field(label = "Document ID", required)]
    res_id: Integer,

    /// Author user id. A plain integer, not a Many2one: res.users is an external table (the auth
    /// subsystem owns `meshble_user`), and the mail subsystem deliberately avoids hard FKs to
    /// volatile actor tables — a deleted user's messages survive with a dangling author id (as in
    /// Odoo's `ondelete='set null'`). The UI resolves the name from res.users when displaying.
    #[field(label = "Author")]
    author_id: Integer,

    #[field(label = "Type", default = "comment", selection = "comment:Comment,notification:Notification,note:Log note")]
    message_type: Selection,

    #[field(label = "Body")]
    body: Text,

    #[field(label = "Date")]
    date: Datetime,

    /// Threaded replies (self-referential). Top-level messages have no parent.
    #[field(label = "Parent Message", target = "mail.message")]
    parent_id: Many2one,
}

/// A field-change audit row: one tracked field went from `old_value` to `new_value`, carried by a
/// `notification` message. ONE typed value pair (serialized from the field's `Value`), not Odoo's
/// ~10-column sparse table. `message_id` is a plain integer (no FK), like `author_id`: the subsystem
/// avoids hard FKs, and the record-delete cleanup removes a record's tracking via its messages.
#[model(name = "mail.tracking", table = "mail_tracking")]
pub struct MailTracking {
    #[field(label = "Message", required)]
    message_id: Integer,

    #[field(label = "Field", required)]
    field: Text,

    #[field(label = "Old Value")]
    old_value: Text,

    #[field(label = "New Value")]
    new_value: Text,
}

/// A scheduled to-do on a record (Odoo's `mail.activity`): a deadline + an assignee, attached via the
/// polymorphic `(res_model, res_id)` link. The `state` (overdue/today/planned) is DERIVED from
/// `date_deadline` at read time — never stored — so it can't drift and needs no "aging" cron (Odoo
/// derives it too, but writes the rule THREE times; here it lives once, in the activities endpoint).
/// `user_id` is a plain integer (assignee), FK-light like `author_id`. Done = `active` goes false.
#[model(name = "mail.activity", table = "mail_activity")]
pub struct MailActivity {
    #[field(label = "Document Model", required)]
    res_model: Text,

    #[field(label = "Document ID", required)]
    res_id: Integer,

    #[field(label = "Summary", required)]
    summary: Text,

    #[field(label = "Deadline")]
    date_deadline: Date,

    #[field(label = "Assigned To")]
    user_id: Integer,

    #[field(label = "Open", default = "true")]
    active: Bool,
}

/// A subscription: a user follows a record's thread (Odoo's `mail.followers`), via the polymorphic
/// `(res_model, res_id)` link. `user_id` is a plain integer (FK-light, like `author_id`). Uniqueness
/// of `(res_model, res_id, user_id)` is enforced by a composite index (see `ensure_mail_indexes`) so
/// following is idempotent. Per-subtype opt-in (Odoo's notorious complexity) is deferred: v1 = follow-all.
#[model(name = "mail.follower", table = "mail_follower")]
pub struct MailFollower {
    #[field(label = "Document Model", required)]
    res_model: Text,

    #[field(label = "Document ID", required)]
    res_id: Integer,

    #[field(label = "Follower", required)]
    user_id: Integer,
}

/// Mail ACLs: only `admin` touches `mail.message` through the GENERIC CRUD routes (moderation/debug).
/// Normal users never read or post messages directly — that would expose every record's thread across
/// all companies, bypassing host visibility. Instead the dedicated chatter endpoints gate on the
/// caller's read access to the HOST record and then act on `mail.message` with elevated rights. So a
/// user with no mail ACL still posts/reads threads, but only of records they can already see.
pub static ACLS: &[Acl] = &[
    Acl { model: "mail.message", group: "admin", read: true, write: false, create: true, delete: true },
    Acl { model: "mail.tracking", group: "admin", read: true, write: false, create: false, delete: true },
    Acl { model: "mail.activity", group: "admin", read: true, write: true, create: true, delete: true },
    Acl { model: "mail.follower", group: "admin", read: true, write: false, create: true, delete: true },
];
meshble::register_acls!(ACLS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_manifest_is_compatible() {
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn message_model_resolves_with_polymorphic_columns() {
        let m = resolve_registered("mail.message").unwrap();
        let ddl = to_ddl(&m);
        assert!(ddl.contains("CREATE TABLE mail_message"));
        assert!(ddl.contains("res_model text NOT NULL"));
        assert!(ddl.contains("res_id bigint NOT NULL"));
        // Polymorphic: res_id is a bare bigint, NOT a foreign key.
        assert!(!ddl.contains("res_id bigint NOT NULL REFERENCES"));
        // parent_id IS a real self-FK (threading within one table).
        assert!(ddl.contains("REFERENCES mail_message(id)"));
    }

    #[test]
    fn tracking_and_activity_models_resolve() {
        let tr = to_ddl(&resolve_registered("mail.tracking").unwrap());
        assert!(tr.contains("CREATE TABLE mail_tracking"));
        // message_id is FK-light (plain bigint, no REFERENCES) — like author_id.
        assert!(tr.contains("message_id bigint NOT NULL"));
        assert!(!tr.contains("REFERENCES"));

        let act = to_ddl(&resolve_registered("mail.activity").unwrap());
        assert!(act.contains("CREATE TABLE mail_activity"));
        assert!(act.contains("date_deadline date"));
        assert!(act.contains("res_id bigint NOT NULL"));
        // state is NOT a column — it is derived at read time from date_deadline.
        assert!(!act.contains(" state "));

        let foll = to_ddl(&resolve_registered("mail.follower").unwrap());
        assert!(foll.contains("CREATE TABLE mail_follower"));
        assert!(foll.contains("user_id bigint NOT NULL"));
        // FK-light: user_id is a plain integer; uniqueness is a composite index, not a column UNIQUE.
        assert!(!foll.contains("REFERENCES"));
    }
}
