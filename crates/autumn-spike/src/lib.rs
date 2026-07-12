//! Isolated migration spike: AletheiaDB's `GET /nodes/{id}` on autumn-web 0.5.0.
//!
//! This crate is a **vertical slice** proving that one authenticated endpoint
//! can be expressed in autumn 0.5's idioms — `#[get("/nodes/{id}")]` +
//! `#[api_doc(mcp)]` — and projected simultaneously to **HTTP**,
//! **MCP-over-HTTP** (`/mcp`), and **OpenAPI** (`/openapi.json`), while
//! preserving AletheiaDB's cross-cutting guarantees: bearer auth (constant-time,
//! on-by-default), RBAC, the structured flat error envelope, and off-executor
//! DB access.
//!
//! # Why a separate crate?
//!
//! autumn-macros 0.5 expands its route/`api_doc` macros to hard-coded absolute
//! paths (`::autumn_web::…`) with no `proc-macro-crate` rename support, so the
//! 0.5 macros can only be used where `autumn_web` in the extern prelude *is*
//! the 0.5 crate. The main `aletheiadb` crate is pinned to autumn 0.4 (whose
//! 0.4 macros likewise require `::autumn_web` = 0.4), and a single crate cannot
//! bind the name `autumn_web` to two versions at once. An isolated crate that
//! depends on autumn-web 0.5 **unrenamed** sidesteps this entirely and keeps
//! the main crate's gates untouched. See
//! `docs/spikes/autumn-web-migration-spike.md`.
//!
//! The shared AletheiaDB core (constant-time `AuthStore`, `AccessClass`/`Role`
//! RBAC, the `POST /query` `GetNode` node serializer, `get_node`) is reused via
//! the `aletheiadb` path dependency, so the parity test is a true byte-equality
//! assertion against the existing `POST /query` path.

#![warn(missing_docs)]

pub mod app;
pub mod auth;
pub mod handler;
pub mod state;

pub use app::{DEMO_MIDDLEWARE_HEADER, build_spike_client, build_spike_testapp};
pub use auth::{AuthStoreTokenAdapter, SpikeAuth, SpikeAuthState};
pub use handler::get_node;
pub use state::SpikeState;

/// The spike reuses AletheiaDB's existing HTTP error envelope verbatim, so its
/// error responses are byte-identical to the current `POST /query` surface.
pub use aletheiadb::http::AletheiaHttpError;
