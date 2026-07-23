//! Asynchronous replication engine (Issue #3355, Slice B).
//!
//! Pull-based WAL shipping: a replica polls a primary's [`feed::ReplicationFeed`]
//! for durable (flushed-to-segment) WAL entries starting at its own applied
//! LSN, applies only whole commit frames (never a torn transaction), and
//! publishes its applied position for lag observability. This module is the
//! engine; [`crate::db::replication_role`] (Slice A) is the always-compiled
//! role/enforcement core it plugs into, and the public start/bootstrap API
//! lives in `crate::db::replication` (a separate file, Slice B).
//!
//! # Layout
//!
//! - [`feed`] — the primary-side [`feed::ReplicationFeed`]: serves durable WAL
//!   entries from a given LSN, or a structured resync signal when the
//!   requested LSN has fallen off the retained WAL segments.
//! - [`source`] — the [`source::ReplicationSource`] trait a replica pulls
//!   from, plus [`source::InProcessSource`] (the in-process/test/chaos
//!   transport used until Slice C's TCP client lands).
//! - [`apply`] — the live-apply wrapper: turns a raw fetched entry stream into
//!   the longest *complete* commit-frame prefix, applies it to a running
//!   replica's storage via the shared recovery replay engine, and performs
//!   the PITR-style post-apply bookkeeping (id generators, temporal index,
//!   HLC seed).
//! - [`applier`] — [`applier::ReplicaApplierHandle`]: the background thread
//!   that owns a [`source::ReplicationSource`] and drives the fetch → apply →
//!   publish-progress → periodic-persist loop.
//!
//! # Always compiled, std-only
//!
//! Per the design, this engine (feed/source/apply/applier) carries no feature
//! flag of its own and pulls in no new dependency -- only the (future) TCP
//! transport (Slice C) is expected to gate real network I/O behind a
//! `replication` feature. Absent on the wasm32 ephemeral profile, like the
//! rest of the durability stack it builds on (WAL segment files, index
//! persistence).

pub mod applier;
pub(crate) mod apply;
pub mod feed;
pub mod source;

pub use applier::ReplicationOptions;
pub use feed::{FetchOutcome, ReplicationFeed};
pub use source::{InProcessSource, ReplicationSource};
