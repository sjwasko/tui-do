//! What a push pass knows about the outbox while it drains it.
//!
//! BUG-23: the drain used to re-read the whole queue with `Store::pending` before every
//! entry, parsing every payload each time -- 1,586,871 parses to drain 1,781 entries, and
//! 16.7 s of the store's one lock. [`DrainQueue`] keeps the queue's shape instead
//! ([`Queued`]: id, subject, backoff, no payload) and asks only for what can have changed:
//! rows queued since the last look, by id. A payload is parsed when its entry is about to
//! be sent, or when a blocked label means the scan has to ask `references` -- once per
//! entry per pass either way.
//!
//! **Only the drain rewrites or deletes a queued row.** The interface and `tui-do add`
//! append; `complete`, `settle_create`, `settle_create_label` and `discard_all` are called
//! from `Sync::push_with` alone, and one pass runs at a time. So what this holds goes stale
//! in exactly the places the drain can see: a settle rewrites later payloads and subjects
//! (`retarget`, `retarget_label`), and a rejection discards a set. The drain calls
//! [`DrainQueue::mark_stale`] for both, and the next [`DrainQueue::refresh`] reads the
//! whole shape again and forgets every payload it had parsed. Debug builds check the
//! invariant on every refresh.

use std::collections::HashMap;

use crate::error::Result;
use crate::store::{Mutation, Queued, Store};

/// The outbox as a push pass last saw it.
#[derive(Debug)]
pub(super) struct DrainQueue {
    /// Every row still queued, oldest first, as of the last refresh -- less what the
    /// drain has settled since.
    rows: Vec<Queued>,
    /// The highest id read. Ids only grow, so anything above it is new.
    seen: i64,
    /// Whether rows or payloads may have been rewritten since they were read.
    stale: bool,
    /// Payloads parsed for the `references` scan, by id.
    carried: HashMap<i64, Mutation>,
}

impl DrainQueue {
    /// Knows nothing yet, so the first refresh reads everything.
    pub(super) fn new() -> Self {
        Self {
            rows: Vec::new(),
            seen: 0,
            stale: true,
            carried: HashMap::new(),
        }
    }

    /// Catch up with the store: everything, if stale, or only what was queued since.
    pub(super) async fn refresh(&mut self, store: &Store) -> Result<()> {
        if self.stale {
            self.rows = store.queued_after(0).await?;
            self.carried.clear();
            self.stale = false;
        } else {
            if cfg!(debug_assertions) {
                // Rows at or below `seen` change only through the drain, which forgets
                // what it settles and marks the rest stale. A count that disagrees means
                // something else deleted a queued row out from under the pass.
                let held = store.pending_through(self.seen).await?;
                debug_assert_eq!(
                    usize::try_from(held).unwrap_or(usize::MAX),
                    self.rows.len(),
                    "a queued row changed outside the drain"
                );
            }
            self.rows.extend(store.queued_after(self.seen).await?);
        }
        if let Some(last) = self.rows.last() {
            self.seen = self.seen.max(last.id);
        }
        Ok(())
    }

    /// How many entries are queued, as far as this pass knows.
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    /// The highest id this pass has read.
    pub(super) const fn seen(&self) -> i64 {
        self.seen
    }

    /// Parse the payloads of every row `wanted` picks that has not been parsed yet.
    pub(super) async fn carry(
        &mut self,
        store: &Store,
        wanted: impl Fn(&Queued) -> bool,
    ) -> Result<()> {
        let unread: Vec<i64> = self
            .rows
            .iter()
            .filter(|queued| !self.carried.contains_key(&queued.id) && wanted(queued))
            .map(|queued| queued.id)
            .collect();
        if !unread.is_empty() {
            self.carried.extend(store.mutations(unread).await?);
        }
        Ok(())
    }

    /// Keep a payload already read, so the next scan can see what it references.
    pub(super) fn keep(&mut self, id: i64, mutation: Mutation) {
        self.carried.insert(id, mutation);
    }

    /// The first row, oldest first, that `eligible` takes. It is handed the payload when
    /// one has been parsed.
    pub(super) fn next_candidate(
        &self,
        mut eligible: impl FnMut(&Queued, Option<&Mutation>) -> bool,
    ) -> Option<i64> {
        self.rows
            .iter()
            .find(|queued| eligible(queued, self.carried.get(&queued.id)))
            .map(|queued| queued.id)
    }

    /// The store no longer holds this row.
    pub(super) fn forget(&mut self, id: i64) {
        self.rows.retain(|queued| queued.id != id);
        self.carried.remove(&id);
    }

    /// Rows or payloads may have been rewritten; read them all again next time.
    pub(super) fn mark_stale(&mut self) {
        self.stale = true;
    }
}
