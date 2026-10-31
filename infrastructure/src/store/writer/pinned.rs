//! Pinning senders (issue #26, ADR-024). A pin changes which rows lead every Inbox and which
//! rows show the pin, so each change reloads every open view.

use super::{State, Writer};
use crate::store::api::{ChangeEvent, ViewChange, ViewDiff};
use crate::store::pins::{self, Pin, PinKind, PinMode, PinTarget};
use crate::store::{StoreError, views};
use rusqlite::{Transaction, TransactionBehavior, params};
use sealwax_domain::model::MessageId;
use std::sync::Arc;

impl Writer {
    pub(super) fn pin(
        &mut self,
        target: &PinTarget,
        kind: PinKind,
        at: i64,
    ) -> Result<Option<Pin>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(key) = pins::key(&tx, target, kind)? else {
            return Ok(None);
        };
        let added = tx
            .prepare_cached(
                "INSERT INTO pinned_senders(kind, key, mode, added) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(kind, key) DO NOTHING",
            )?
            .execute(params![
                pins::kind_code(kind),
                key,
                pins::mode_code(PinMode::default()),
                at
            ])?;
        let pin = tx
            .prepare_cached("SELECT mode, added FROM pinned_senders WHERE kind = ?1 AND key = ?2")?
            .query_row(params![pins::kind_code(kind), key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
        let pin = Pin {
            kind,
            key,
            mode: if pin.0 == pins::mode_code(PinMode::Keep) {
                PinMode::Keep
            } else {
                PinMode::UntilRead
            },
            added: pin.1,
        };
        if added > 0 {
            commit_pins(&mut self.state, tx)?;
        }
        Ok(Some(pin))
    }

    pub(super) fn unpin(&mut self, message: MessageId) -> Result<Vec<Pin>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = pins::of_message(&tx, message)?;
        if removed.is_empty() {
            return Ok(removed);
        }
        {
            let mut delete =
                tx.prepare_cached("DELETE FROM pinned_senders WHERE kind = ?1 AND key = ?2")?;
            for pin in &removed {
                delete.execute(params![pins::kind_code(pin.kind), pin.key])?;
            }
        }
        commit_pins(&mut self.state, tx)?;
        Ok(removed)
    }

    pub(super) fn restore_pins(&mut self, restored: &[Pin]) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO pinned_senders(kind, key, mode, added) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(kind, key) DO UPDATE SET mode = excluded.mode \
                 WHERE mode <> excluded.mode",
            )?;
            for pin in restored.iter().filter(|pin| !pin.key.is_empty()) {
                changed += upsert.execute(params![
                    pins::kind_code(pin.kind),
                    pin.key,
                    pins::mode_code(pin.mode),
                    pin.added
                ])?;
            }
        }
        if changed > 0 {
            commit_pins(&mut self.state, tx)?;
        }
        Ok(())
    }

    pub(super) fn remove_pin(&mut self, kind: PinKind, key: &str) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx
            .prepare_cached("DELETE FROM pinned_senders WHERE kind = ?1 AND key = ?2")?
            .execute(params![pins::kind_code(kind), key])?;
        if removed > 0 {
            commit_pins(&mut self.state, tx)?;
        }
        Ok(())
    }
}

/// Commits a change to the pins: every open view reloads.
fn commit_pins(state: &mut State, tx: Transaction<'_>) -> Result<(), StoreError> {
    let shared = Arc::clone(&state.shared);
    let open = shared.registry.lock();
    let mut changes = Vec::new();
    for (view, spec) in &open.open {
        let folders = views::folders(&tx, spec.scope)?;
        changes.push(ViewChange {
            view: *view,
            diff: ViewDiff::Reload {
                total: views::count(&tx, &folders, spec.filter)?,
            },
        });
    }
    let event = ChangeEvent {
        views: changes,
        ..ChangeEvent::default()
    };
    state.commit(tx, event)?;
    drop(open);
    Ok(())
}
