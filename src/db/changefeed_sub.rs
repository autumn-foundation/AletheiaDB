//! Push-changefeed subscription API on [`AletheiaDB`] (Issue #3375).
//!
//! The [`AletheiaDB::list_changes`](crate::AletheiaDB::list_changes) *pull* feed answers
//! "what changed between T1 and T2?" on demand. This module adds the complementary *push*
//! feed: [`subscribe_changes`](AletheiaDB::subscribe_changes) hands out a
//! [`Subscription`] whose buffer fills with matching [`ChangeRecord`](crate::ChangeRecord)s
//! as transactions commit — no polling.
//!
//! # No-loss contract
//!
//! Delivery is **best-effort at-least-once**; the *durable* ground truth is `list_changes`.
//! A consumer that lags (buffer overflow → disconnected), reconnects, or survives a crash
//! resumes with **zero loss** by pulling `list_changes` from its last
//! [`resume_token`](Subscription::resume_token) — see
//! [`docs/guides/reacting-to-change.md`](https://example/) and
//! [`crate::core::changefeed_subscription`] for the full contract (including
//! duplicate-on-resume, deduped by the stable `ChangeRecord` cursor).
//!
//! The HTTP SSE surface and the MCP `await_changes` tool that wrap the blocking
//! [`Subscription::recv_timeout`] long-poll are a coordinated follow-up (Lane 1).

use crate::core::changefeed_subscription::{ChangeFilter, ChangefeedConfig, Subscription};
use crate::core::error::Result;
use crate::db::AletheiaDB;

impl AletheiaDB {
    /// Subscribe to the push changefeed, receiving every committed change matching `filter`.
    ///
    /// The returned [`Subscription`] buffers matching changes as transactions commit; drain
    /// it with [`Subscription::poll`] (non-blocking) or [`Subscription::recv_timeout`]
    /// (blocking long-poll). Dropping the subscription deregisters it.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::CapacityExceeded`](crate::StorageError::CapacityExceeded) when
    /// the configured maximum number of concurrent subscriptions is already reached.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::{AletheiaDB, ChangeFilter};
    /// use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let sub = db.subscribe_changes(ChangeFilter::all())?;
    ///
    /// db.write(|tx| { tx.create_node("Person", aletheiadb::PropertyMap::new())?; Ok::<_, aletheiadb::Error>(()) })?;
    ///
    /// let events = sub.poll();
    /// assert_eq!(events.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the returned Subscription must be held to keep receiving changes"]
    pub fn subscribe_changes(&self, filter: ChangeFilter) -> Result<Subscription> {
        self.changefeed.subscribe(filter)
    }

    /// Reconfigure the changefeed caps (max concurrent subscriptions and per-subscription
    /// buffer capacity). The subscription cap takes effect immediately; the buffer capacity
    /// applies to subscriptions created afterward (existing ones keep their capacity).
    pub fn set_changefeed_config(&self, config: ChangefeedConfig) {
        self.changefeed.set_config(config);
    }

    /// The number of currently-live changefeed subscriptions (primarily for tests/metrics).
    pub fn changefeed_subscription_count(&self) -> usize {
        self.changefeed.subscription_count()
    }
}
