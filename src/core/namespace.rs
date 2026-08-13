//! Agent-scoped memory namespaces — core model (Issue #3349, PR1).
//!
//! A **namespace** is a stable string label on the *ownership / visibility*
//! axis, orthogonal to the type `label` axis. A node's `label` says *what kind
//! of thing* it is (`Person`); its `namespace` says *whose scope it lives in*
//! (`agent:planner`). Every node/edge belongs to exactly one namespace, fixed
//! at creation and **immutable for the life of the entity**.
//!
//! # Storage: reserved-key ride-along (no WAL format change)
//!
//! Following the #3596 (`replace_*`) and #3604 (CAS/lease) precedent, the
//! namespace is persisted as a **reserved, engine-owned property-map entry**
//! ([`NAMESPACE_KEY`]) written through the *existing*
//! `CreateNode`/`UpdateNode`/`CreateEdge`/`UpdateEdge` ops — no new
//! `WalOperation` field and no `WAL_VERSION` bump. Because it rides an existing
//! property map, it round-trips for free through WAL replay, anchor/delta
//! reconstruction, cold-tier migration, and `.albk` backup. A legacy entity
//! simply lacks the key ⇒ it resolves to [`Namespace::DEFAULT`].
//!
//! To keep the `default` namespace byte-identical to pre-namespace data (and to
//! preserve exact back-compat for omitted-namespace writes), the ride-along key
//! is stamped **only for non-default namespaces**; a `default` entity carries no
//! key at all, exactly like a legacy one.
//!
//! # Reserved property-key policy
//!
//! The **entire** `__aletheia_*` property-key prefix is reserved for
//! engine-owned ride-along keys (not just [`NAMESPACE_KEY`]). The `__shred_*`
//! prefix is likewise reserved for the GDPR crypto-shred lane. Any **user**
//! write whose property map contains a key with either prefix is rejected with
//! a structured [`NamespaceError::ReservedPropertyKey`] (→ MCP
//! `INVALID_ARGUMENT`) at the central write-validation seam, so a caller can
//! never forge or overwrite a namespace marker. Engine-reserved keys are
//! **elided** from every user-facing property view and surfaced instead as a
//! first-class `namespace` field.

use crate::core::hasher::IdentityHasher;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use dashmap::DashMap;
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// The engine-owned property key under which an entity's namespace rides along
/// its property map. Reserved: rejected on user writes, elided from user-facing
/// views. See the [module docs](self).
pub const NAMESPACE_KEY: &str = "__aletheia_ns";

/// Reserved engine-owned property-key prefix. The **entire** prefix is reserved
/// (Issue #3349 coordinator ruling), not just [`NAMESPACE_KEY`].
pub const ALETHEIA_RESERVED_PREFIX: &str = "__aletheia_";

/// Reserved property-key prefix owned by the GDPR crypto-shred lane. User
/// writes carrying it are rejected here so the two lanes never collide.
pub const SHRED_RESERVED_PREFIX: &str = "__shred_";

/// The reserved **fail-closed sentinel** namespace value (Issue #3349, PR3c
/// security fix).
///
/// A changefeed record is stamped with this namespace **only** when the engine
/// genuinely cannot derive the affected entity's real namespace — e.g. a
/// delta-chain reconstruction hard-failure ([`crate::core::error::TemporalError::MaxDepthExceeded`]
/// / `MissingAnchor`). It is deliberately **not** a creatable namespace:
/// [`Namespace::new`] rejects the `__aletheia_*` prefix, so no user scope can
/// ever name it. Consequently [`NamespaceScope::contains`] returns `false` for
/// it under `Single(_)` / `List(_)` / the default scope (it can never leak to a
/// user-scoped read or subscriber) but `true` under [`NamespaceScope::All`] and
/// for an unset scope (so an unscoped `list_changes` never silently drops it).
///
/// Deriving to this sentinel — **never** to [`Namespace::DEFAULT`] — on a
/// derivation miss/failure is the security-critical contract: `default` is a
/// real, subscribable namespace, so failing open to it would both leak a
/// non-default entity's change to `default`-scoped subscribers and hide it from
/// its real namespace's subscribers.
pub const UNRESOLVED_NAMESPACE: &str = "__aletheia_unresolved";

/// Maximum length of a namespace identifier, in bytes.
pub const MAX_NAMESPACE_LEN: usize = 128;

/// The read-selector keyword that is **not** a creatable namespace name.
const ALL_SELECTOR: &str = "all";

/// Whether a property key is engine-reserved (owned by the namespace ride-along
/// or the crypto-shred lane) and therefore forbidden on user writes and elided
/// from user-facing views.
///
/// This is the single source of truth for the reserved-prefix policy; wire it
/// into every place that either validates an incoming user property map or
/// renders one to a user-facing payload.
#[inline]
#[must_use]
pub fn is_reserved_property_key(key: &str) -> bool {
    key.starts_with(ALETHEIA_RESERVED_PREFIX) || key.starts_with(SHRED_RESERVED_PREFIX)
}

/// A validated namespace identifier (Issue #3349).
///
/// Construct via [`Namespace::new`] (validating) or [`Namespace::default`] (the
/// reserved [`Namespace::DEFAULT`] namespace). Identifier rules: non-empty, at
/// most [`MAX_NAMESPACE_LEN`] bytes, charset `[A-Za-z0-9._:/-]`, case-sensitive.
/// The read-selector keyword `all` is **not** a creatable name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Namespace(String);

impl Namespace {
    /// The implicit default namespace. Any entity created without an explicit
    /// namespace — and every legacy entity carrying no ride-along key — lives
    /// here.
    pub const DEFAULT: &'static str = "default";

    /// Validate and construct a namespace from a string.
    ///
    /// # Errors
    ///
    /// Returns [`NamespaceError::InvalidName`] (→ MCP `INVALID_ARGUMENT`, with
    /// the offending value carried in the error) when `name` is empty, exceeds
    /// [`MAX_NAMESPACE_LEN`] bytes, contains a character outside
    /// `[A-Za-z0-9._:/-]`, or is the reserved read-selector keyword `all`.
    pub fn new(name: impl Into<String>) -> Result<Self, NamespaceError> {
        let name = name.into();
        if name.is_empty() {
            return Err(NamespaceError::InvalidName {
                name,
                reason: "namespace must not be empty".to_string(),
            });
        }
        if name.len() > MAX_NAMESPACE_LEN {
            return Err(NamespaceError::InvalidName {
                reason: format!(
                    "namespace must be at most {MAX_NAMESPACE_LEN} bytes (got {})",
                    name.len()
                ),
                name,
            });
        }
        if name == ALL_SELECTOR {
            return Err(NamespaceError::InvalidName {
                name,
                reason: "'all' is a read-scope selector, not a creatable namespace".to_string(),
            });
        }
        // Reject the engine-owned reserved prefixes as namespace *names* (Issue
        // #3349 hardening), symmetric with the property-key reservation: a
        // namespace called `__aletheia_ns` / `__shred_x` would collide with the
        // ride-along marker space. Reuse the single reserved-prefix predicate.
        if is_reserved_property_key(&name) {
            return Err(NamespaceError::InvalidName {
                reason: format!(
                    "namespace must not use an engine-reserved prefix ('{ALETHEIA_RESERVED_PREFIX}' \
                     or '{SHRED_RESERVED_PREFIX}')"
                ),
                name,
            });
        }
        if let Some(bad) = name
            .chars()
            .find(|c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | ':' | '/' | '-'))
        {
            return Err(NamespaceError::InvalidName {
                reason: format!(
                    "namespace contains invalid character {bad:?}; allowed charset is \
                     [A-Za-z0-9._:/-]"
                ),
                name,
            });
        }
        Ok(Namespace(name))
    }

    /// The namespace as a string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the implicit [`Namespace::DEFAULT`] namespace.
    #[inline]
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }

    /// Consume into the owned string.
    #[inline]
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Construct the reserved [`UNRESOLVED_NAMESPACE`] fail-closed sentinel,
    /// **bypassing** the prefix/charset validation [`Namespace::new`] enforces
    /// (which rejects the `__aletheia_*` prefix). This is `pub(crate)` and is the
    /// only sanctioned way to mint an engine-reserved namespace value: it must
    /// never be exposed to user construction, or a caller could forge a scope
    /// that matches the sentinel.
    #[must_use]
    pub(crate) fn unresolved() -> Self {
        Namespace(UNRESOLVED_NAMESPACE.to_string())
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Namespace(Self::DEFAULT.to_string())
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Namespace {
    type Err = NamespaceError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Namespace::new(s)
    }
}

/// Structured namespace errors, mapped to the #3234 MCP error codes at the MCP
/// boundary (see `crate::mcp::error`). All are caller faults and never
/// retriable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NamespaceError {
    /// A namespace name failed validation (empty, too long, bad charset, or the
    /// reserved `all` selector). Maps to `INVALID_ARGUMENT`; the offending
    /// `name` is carried for the caller.
    #[error("invalid namespace name {name:?}: {reason}")]
    InvalidName {
        /// The offending value.
        name: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A referenced namespace is not registered. Maps to `NOT_FOUND`; the
    /// `namespace` name is carried for the caller.
    #[error("namespace {namespace:?} not found")]
    NotFound {
        /// The missing namespace name.
        namespace: String,
    },
    /// A `create_namespace` collided with an existing registration (including
    /// the implicit `default`). Maps to `CONFLICT`.
    #[error("namespace {namespace:?} already exists")]
    AlreadyExists {
        /// The conflicting namespace name.
        namespace: String,
    },
    /// A user write carried an engine-reserved property key (`__aletheia_*` or
    /// `__shred_*`). Maps to `INVALID_ARGUMENT`.
    #[error(
        "property key {key:?} is engine-reserved (the '__aletheia_*' and '__shred_*' prefixes \
         are reserved) and may not be set by a user write"
    )]
    ReservedPropertyKey {
        /// The offending reserved key.
        key: String,
    },
    /// A namespace was explicitly supplied to a non-create write
    /// (update / replace / CAS / lease-claim). A namespace is fixed at creation,
    /// so passing one to an update-class op is a caller error rather than a
    /// silent no-op. Maps to `INVALID_ARGUMENT`.
    #[error("namespace is immutable and cannot be changed after creation")]
    Immutable,
    /// A programmatic namespace scope (a request parameter) and an in-query
    /// `USE / IN NAMESPACE` clause were both supplied and are **not**
    /// semantically identical (Issue #3349). Rather than silently pick a winner
    /// — which could widen an intended restriction and leak another agent's
    /// facts — the combination is refused; the caller must specify the scope in
    /// exactly one place, or make the two identical. Maps to `INVALID_ARGUMENT`.
    ///
    /// The offending namespace names are deliberately **not** carried on this
    /// error, so an isolation boundary is never disclosed through the error
    /// surface.
    #[error(
        "in-query namespace clause conflicts with the namespace request parameter; supply only \
         one, or make them identical"
    )]
    ScopeConflict,
}

// ============================================================================
// Read scope (Issue #3349, PR2)
// ============================================================================

/// A namespace read scope: which namespaces a scoped read is allowed to see
/// (Issue #3349).
///
/// - [`Single`](Self::Single): exactly one namespace.
/// - [`List`](Self::List): the **union** of a non-empty set of namespaces
///   (a read sees entities in any of them and may traverse within and between
///   them).
/// - [`All`](Self::All): every namespace — no filtering (the `all`
///   read-selector keyword).
///
/// The default scope is `Single(`[`Namespace::DEFAULT`]`)`, so an omitted
/// scope reproduces exact single-agent back-compat over `default`-namespace
/// data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamespaceScope {
    /// Exactly one namespace.
    Single(Namespace),
    /// The union of a non-empty list of namespaces.
    List(Vec<Namespace>),
    /// Every namespace (no filtering).
    All,
}

impl Default for NamespaceScope {
    /// The default scope is the `default` namespace only — exact back-compat
    /// for omitted-scope reads over legacy / single-agent data.
    fn default() -> Self {
        NamespaceScope::Single(Namespace::default())
    }
}

impl NamespaceScope {
    /// A scope over exactly one namespace.
    #[must_use]
    pub fn single(namespace: Namespace) -> Self {
        NamespaceScope::Single(namespace)
    }

    /// A scope over every namespace (no filtering).
    #[must_use]
    pub fn all() -> Self {
        NamespaceScope::All
    }

    /// A union scope over a non-empty list of namespaces.
    ///
    /// # Errors
    ///
    /// [`NamespaceError::InvalidName`] (→ MCP `INVALID_ARGUMENT`) with an empty
    /// `name` when `namespaces` is empty — an empty scope would silently match
    /// nothing, which the never-silently-wrong contract forbids.
    pub fn list(namespaces: Vec<Namespace>) -> Result<Self, NamespaceError> {
        if namespaces.is_empty() {
            return Err(NamespaceError::InvalidName {
                name: String::new(),
                reason: "namespace scope list must not be empty".to_string(),
            });
        }
        Ok(NamespaceScope::List(namespaces))
    }

    /// Whether `namespace` is inside this scope.
    #[must_use]
    pub fn contains(&self, namespace: &Namespace) -> bool {
        match self {
            NamespaceScope::All => true,
            NamespaceScope::Single(ns) => ns == namespace,
            NamespaceScope::List(list) => list.iter().any(|ns| ns == namespace),
        }
    }

    /// Resolve this scope to interned [`NamespaceId`]s for fast per-edge
    /// membership probing (Issue #3349, PR2), or `None` for [`All`](Self::All)
    /// (which is a no-op filter). Resolve **once per query** and reuse across all
    /// edges/candidates so the interner's string lookup is paid once, not per
    /// edge. An empty [`List`](Self::List) resolves to `Some(vec![])`, a
    /// match-nothing scope (it should already have been rejected by
    /// `validate_scope`).
    #[must_use]
    pub fn resolved_ids(&self) -> Option<Vec<NamespaceId>> {
        match self {
            NamespaceScope::All => None,
            NamespaceScope::Single(ns) => Some(vec![intern_namespace(ns)]),
            NamespaceScope::List(list) => Some(list.iter().map(intern_namespace).collect()),
        }
    }

    /// Resolve this scope to interned ids **once per query**, carrying the
    /// single-namespace case inline (Issue #3349, PR2 performance hoist).
    ///
    /// Prefer this over [`resolved_ids`](Self::resolved_ids) on the scoped read
    /// hot paths (traversal boundary, source-leaf filter): the result is built
    /// once at the start of a scoped read and threaded down into every per-edge /
    /// per-row membership check, so no probe re-derives ids (no interner string
    /// lookup) or re-allocates. A [`Single`](Self::Single) scope — and a
    /// one-element [`List`](Self::List) — resolves to
    /// [`ResolvedScope::Single`], a single inline [`NamespaceId`] with no `Vec` /
    /// `Arc` allocation, turning the per-edge check into a single integer
    /// compare. An empty `List` (which `validate_scope` already rejects) resolves
    /// to an empty [`ResolvedScope::List`], a match-nothing scope.
    #[must_use]
    pub fn resolve(&self) -> ResolvedScope {
        match self {
            NamespaceScope::All => ResolvedScope::All,
            NamespaceScope::Single(ns) => ResolvedScope::Single(intern_namespace(ns)),
            NamespaceScope::List(list) => match list.as_slice() {
                // A one-element union takes the inline single-namespace fast path.
                [one] => ResolvedScope::Single(intern_namespace(one)),
                // Two or more (or the already-rejected empty) resolve to a shared
                // id set scanned with `any`.
                many => ResolvedScope::List(many.iter().map(intern_namespace).collect()),
            },
        }
    }

    /// The concrete namespaces named by this scope, or `None` for
    /// [`All`](Self::All) (which names no finite set). Deduplicated iteration
    /// helper for callers that enumerate members per namespace.
    #[must_use]
    pub fn explicit_namespaces(&self) -> Option<Vec<Namespace>> {
        match self {
            NamespaceScope::All => None,
            NamespaceScope::Single(ns) => Some(vec![ns.clone()]),
            NamespaceScope::List(list) => {
                let mut seen = std::collections::BTreeSet::new();
                let mut out = Vec::with_capacity(list.len());
                for ns in list {
                    if seen.insert(ns.clone()) {
                        out.push(ns.clone());
                    }
                }
                Some(out)
            }
        }
    }

    /// Whether this scope and `other` select **exactly the same set** of
    /// namespaces (Issue #3349).
    ///
    /// Comparison is by resolved membership set, so it is order-insensitive and
    /// duplicate-insensitive for a [`List`](Self::List) (reusing
    /// [`explicit_namespaces`](Self::explicit_namespaces)'s dedup), and a
    /// `Single(x)` compares equal to a `List([x])` (both select `{x}`).
    /// [`All`](Self::All) — the "every namespace, no filter" sentinel — is only
    /// equal to another `All`; it is never equal to any finite scope, so a
    /// finite scope can never be treated as widening to `All`. Namespace-name
    /// equality is the codebase's existing exact, case-sensitive
    /// [`Namespace`] equality (no separate normalization is invented here).
    #[must_use]
    pub fn same_scope(&self, other: &NamespaceScope) -> bool {
        match (self.explicit_namespaces(), other.explicit_namespaces()) {
            // `All` vs `All`.
            (None, None) => true,
            // `All` vs a finite scope (or vice-versa) — never equal.
            (None, Some(_)) | (Some(_), None) => false,
            (Some(a), Some(b)) => {
                let sa: std::collections::BTreeSet<Namespace> = a.into_iter().collect();
                let sb: std::collections::BTreeSet<Namespace> = b.into_iter().collect();
                sa == sb
            }
        }
    }
}

/// Reconcile a programmatic namespace scope (a request-parameter *ceiling*) with
/// an in-query `USE / IN NAMESPACE` clause, producing the single effective read
/// scope (Issue #3349).
///
/// This is the **one** decision point shared by the AQL and Cypher execution
/// paths, so the two surfaces can never diverge (previously AQL last-wins-dropped
/// the clause while Cypher refused any combination). Let `P` be the programmatic
/// scope (`None` when the request `namespace` parameter is omitted) and `C` the
/// in-query clause lowered to a scope (`None` when the statement carries none):
///
/// | `P`      | `C`      | effective                                             |
/// |----------|----------|-------------------------------------------------------|
/// | `None`   | `None`   | `Single("default")` — the fail-closed default         |
/// | `None`   | `Some C` | `C` — the in-query clause governs (incl. `ALL`)        |
/// | `Some P` | `None`   | `P` — unchanged                                        |
/// | `Some P` | `Some C` | `P` iff [`same_scope`](NamespaceScope::same_scope), else [`NamespaceError::ScopeConflict`] |
///
/// The `(Some, Some)` rule is deliberately strict: **any** non-identical
/// combination is refused — including a clause *narrower* than `P` — because we
/// do not compute subset relations, which keeps a widen-by-accident leak
/// impossible. The effective scope is never wider than `P`.
///
/// # Errors
///
/// [`NamespaceError::ScopeConflict`] (→ `INVALID_ARGUMENT`) when both a
/// programmatic scope and an in-query clause are present and they are not
/// semantically identical.
pub fn reconcile_namespace_scope(
    programmatic: Option<NamespaceScope>,
    in_query: Option<NamespaceScope>,
) -> Result<NamespaceScope, NamespaceError> {
    match (programmatic, in_query) {
        (None, None) => Ok(NamespaceScope::default()),
        (None, Some(c)) => Ok(c),
        (Some(p), None) => Ok(p),
        (Some(p), Some(c)) => {
            if p.same_scope(&c) {
                Ok(p)
            } else {
                Err(NamespaceError::ScopeConflict)
            }
        }
    }
}

// ============================================================================
// Namespace interning (Issue #3349, PR2 — performance)
// ============================================================================

/// A small, copyable interned handle for a namespace name (Issue #3349, PR2).
///
/// Scoped reads probe the secondary membership index (`ns_nodes` / `ns_edges`)
/// once per candidate edge/node. Keying that index by a `u32` id — hashed with
/// [`IdentityHasher`] — turns each probe into an integer hash + shard lookup,
/// eliminating the per-edge `String` hashing that keying by
/// [`Namespace`] (a heap string) incurred. A scope is resolved to its ids
/// **once** (via [`NamespaceScope::resolved_ids`]) and then reused across every
/// edge of a traversal, so the interner's own string lookup is paid once per
/// query, not once per edge.
///
/// The id is a pure in-memory acceleration handle: it is derived from the
/// durable ride-along namespace string, never persisted, and stable within a
/// process for a given name (the same name always interns to the same id
/// regardless of the hydration path — write, WAL replay, index-persistence
/// load, cold rehydration — because there is a single derivation site,
/// [`intern_namespace`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NamespaceId(u32);

impl NamespaceId {
    /// The raw interned id.
    #[inline]
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Process-global namespace interner: name ⇄ [`NamespaceId`]. Namespaces are few
/// (one per agent/session), so no capacity cap is imposed.
struct NamespaceInterner {
    /// name → id
    forward: DashMap<Box<str>, u32>,
    /// id → name (for reverse resolution in cold paths, e.g. per-namespace stats)
    reverse: DashMap<u32, Arc<str>, BuildHasherDefault<IdentityHasher>>,
    next: AtomicU32,
}

impl NamespaceInterner {
    fn new() -> Self {
        NamespaceInterner {
            forward: DashMap::new(),
            reverse: DashMap::with_hasher(BuildHasherDefault::default()),
            next: AtomicU32::new(0),
        }
    }

    fn intern(&self, name: &str) -> NamespaceId {
        // Fast path: already interned (no allocation).
        if let Some(id) = self.forward.get(name) {
            return NamespaceId(*id);
        }
        // Slow path: reserve an id under the entry write lock so two threads
        // interning the same new name agree on one id.
        use dashmap::mapref::entry::Entry;
        match self.forward.entry(name.into()) {
            Entry::Occupied(e) => NamespaceId(*e.get()),
            Entry::Vacant(v) => {
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                self.reverse.insert(id, Arc::from(name));
                v.insert(id);
                NamespaceId(id)
            }
        }
    }

    fn resolve(&self, id: NamespaceId) -> Option<Arc<str>> {
        self.reverse.get(&id.0).map(|e| Arc::clone(e.value()))
    }
}

static NAMESPACE_INTERNER: LazyLock<NamespaceInterner> = LazyLock::new(NamespaceInterner::new);

/// Intern a namespace to its process-stable [`NamespaceId`] (Issue #3349, PR2).
#[inline]
#[must_use]
pub fn intern_namespace(namespace: &Namespace) -> NamespaceId {
    NAMESPACE_INTERNER.intern(namespace.as_str())
}

/// The interned [`NamespaceId`] of the reserved [`UNRESOLVED_NAMESPACE`]
/// fail-closed sentinel (Issue #3349, PR3c). Cheap: interning is a fast-path
/// hash lookup after the first call. Used by the changefeed derivation paths to
/// stamp a record whose real namespace cannot be derived, and to detect such
/// records for tier-aware re-derivation.
#[inline]
#[must_use]
pub(crate) fn unresolved_namespace_id() -> NamespaceId {
    intern_namespace(&Namespace::unresolved())
}

/// Resolve a [`NamespaceId`] back to its [`Namespace`] (cold path — used only by
/// per-namespace stats/schema enumeration and changefeed record materialization).
/// Returns `None` for an id that was never interned in this process.
///
/// The interner only ever holds validated names **or** engine-reserved sentinels
/// (e.g. [`UNRESOLVED_NAMESPACE`]). For a sentinel, [`Namespace::new`] would
/// reject the reserved prefix, so we fall back to the raw interned value rather
/// than silently dropping it to `None` (which would let a caller degrade the
/// sentinel to [`Namespace::DEFAULT`] and re-open the fail-open leak).
#[must_use]
pub fn resolve_namespace_id(id: NamespaceId) -> Option<Namespace> {
    NAMESPACE_INTERNER
        .resolve(id)
        .map(|s| Namespace::new(s.as_ref()).unwrap_or_else(|_| Namespace(s.to_string())))
}

/// A [`NamespaceScope`] resolved to interned [`NamespaceId`]s **once per query**
/// (Issue #3349, PR2 performance hoist).
///
/// Built once via [`NamespaceScope::resolve`] at the start of a scoped read and
/// threaded down into every source-leaf filter and the traversal boundary, so
/// the per-edge / per-row membership check never re-derives ids (no interner
/// string lookup) nor allocates. The single-namespace case — a
/// [`Single`](NamespaceScope::Single) scope or a one-element
/// [`List`](NamespaceScope::List) — is carried **inline** as one
/// [`NamespaceId`] (no `Vec` / `Arc` allocation), so its per-edge check is a
/// single integer compare rather than a set scan.
///
/// Cloning is cheap: `All` is a unit, `Single` copies a `u32`, and `List`
/// bumps an `Arc` refcount — so the executor can hand a fresh clone to every
/// source-leaf iterator without re-resolving.
#[derive(Clone, Debug)]
pub enum ResolvedScope {
    /// `All` — no filtering (matches every namespace).
    All,
    /// Exactly one namespace (from `Single` or a one-element `List`): the
    /// per-edge check is a single integer compare.
    Single(NamespaceId),
    /// A union of two or more namespaces: membership is a scan over this small,
    /// shareable id set. May be empty (the already-rejected empty `List`), in
    /// which case it matches nothing.
    List(Arc<[NamespaceId]>),
}

impl ResolvedScope {
    /// Whether this resolved scope imposes no filter (i.e. is [`All`](Self::All)).
    #[inline]
    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, ResolvedScope::All)
    }

    /// Whether an entity is in scope, given a `probe` that answers "is the
    /// entity a member of namespace `id`?".
    ///
    /// [`All`](Self::All) short-circuits to `true` without probing;
    /// [`Single`](Self::Single) runs exactly one probe (the inlined
    /// single-integer-compare fast path); [`List`](Self::List) scans its (small)
    /// id set with `any`. A namespace is immutable and an entity lives in exactly
    /// one, so a single positive probe is conclusive.
    #[inline]
    #[must_use]
    pub fn contains_by<F: Fn(NamespaceId) -> bool>(&self, probe: F) -> bool {
        match self {
            ResolvedScope::All => true,
            ResolvedScope::Single(id) => probe(*id),
            ResolvedScope::List(ids) => ids.iter().any(|id| probe(*id)),
        }
    }
}

// ============================================================================
// Ride-along read/write helpers
// ============================================================================

/// Extract an entity's namespace from its property map (Issue #3349).
///
/// A map missing [`NAMESPACE_KEY`] — every `default` entity and every legacy
/// entity — resolves to [`Namespace::DEFAULT`]. A stored value that somehow
/// fails validation also degrades to the default rather than erroring (the
/// engine controls what it writes, so this is defensive only).
#[must_use]
pub(crate) fn namespace_of(properties: &PropertyMap) -> Namespace {
    match properties
        .get(NAMESPACE_KEY)
        .and_then(PropertyValue::as_str)
    {
        Some(s) => Namespace::new(s).unwrap_or_default(),
        None => Namespace::default(),
    }
}

/// Return the first engine-reserved key present in an incoming **user**
/// property map, if any. Used by the write-validation seam to reject a forged
/// namespace/shred marker before it can be stamped or committed.
#[must_use]
pub(crate) fn first_reserved_key(properties: &PropertyMap) -> Option<String> {
    // Fast path: an empty property map (a common no-property write) can carry no
    // reserved key, so skip the per-key interner-resolve scan entirely.
    if properties.is_empty() {
        return None;
    }
    for (key, _) in properties.iter() {
        if let Some(name) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string())
            && is_reserved_property_key(&name)
        {
            return Some(name);
        }
    }
    None
}

/// Reject an incoming user property map that carries any engine-reserved key.
///
/// This is the central enforcement point for the reserved-prefix policy; call
/// it on the caller-supplied map at every write entry point, **before** the
/// namespace is stamped (so the engine's own stamp is never rejected).
///
/// # Errors
///
/// [`NamespaceError::ReservedPropertyKey`] naming the offending key.
pub(crate) fn reject_reserved_keys(properties: &PropertyMap) -> Result<(), NamespaceError> {
    match first_reserved_key(properties) {
        Some(key) => Err(NamespaceError::ReservedPropertyKey { key }),
        None => Ok(()),
    }
}

/// Reject an explicit namespace supplied to a **non-create** write
/// (update / replace / CAS / lease-claim).
///
/// A namespace is fixed at creation and immutable thereafter; the update-class
/// paths re-stamp it from the existing entity, so an explicit namespace on such
/// a call would otherwise be a **silent no-op success**. Rejecting it up front
/// keeps the surface never-silently-wrong.
///
/// # Errors
///
/// [`NamespaceError::Immutable`] (→ MCP `INVALID_ARGUMENT`) when `namespace` is
/// `Some`.
pub(crate) fn reject_namespace_on_update(
    namespace: Option<&Namespace>,
) -> Result<(), NamespaceError> {
    if namespace.is_some() {
        return Err(NamespaceError::Immutable);
    }
    Ok(())
}

/// Stamp the namespace ride-along key onto a **create** property map.
///
/// The default namespace is deliberately **not** stamped: a `default` entity
/// carries no ride-along key, keeping it byte-identical to a legacy entity so
/// omitted-namespace behavior is unchanged. A non-default namespace is written
/// as a [`PropertyValue::String`] under [`NAMESPACE_KEY`].
#[must_use]
pub(crate) fn stamp_namespace(properties: PropertyMap, namespace: &Namespace) -> PropertyMap {
    if namespace.is_default() {
        return properties;
    }
    PropertyMapBuilder::from_map(properties)
        .insert(NAMESPACE_KEY, PropertyValue::from(namespace.as_str()))
        .build()
}

/// Re-stamp the **immutable** namespace onto a full-overwrite / merged property
/// map, reading it from the entity's existing property map.
///
/// A namespace is fixed at creation and can never be dropped or changed, so
/// every update PATCH, `replace_*`, CAS, and lease-claim re-stamps it from the
/// existing entity. If the existing entity is in the default namespace there is
/// nothing to carry (and the new map already lacks the key).
#[must_use]
pub(crate) fn restamp_namespace(
    new_properties: PropertyMap,
    existing: &PropertyMap,
) -> PropertyMap {
    let ns = namespace_of(existing);
    stamp_namespace(new_properties, &ns)
}

/// Produce a user-facing copy of a property map with **all** engine-reserved
/// keys elided (Issue #3349). The namespace is surfaced separately as a
/// first-class field via [`namespace_of`]; the ride-along key must never leak
/// into a user-facing payload.
///
/// Returns the original map unchanged (a cheap `Arc` clone) when it carries no
/// reserved key — the overwhelmingly common case — so the default namespace
/// pays nothing.
#[must_use]
pub(crate) fn user_facing_properties(properties: &PropertyMap) -> PropertyMap {
    if first_reserved_key(properties).is_none() {
        return properties.clone();
    }
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in properties.iter() {
        let keep = GLOBAL_INTERNER
            .resolve_with(*key, |s| !is_reserved_property_key(s))
            .unwrap_or(true);
        if keep {
            builder = builder.insert_by_key(*key, value.clone());
        }
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn valid_names_accepted() {
        for name in [
            "default",
            "agent:planner",
            "session:2026-07-08-run3",
            "shared",
            "a",
            "A.b_c-d/e:f",
        ] {
            assert!(Namespace::new(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn empty_name_rejected_with_value() {
        let err = Namespace::new("").unwrap_err();
        match err {
            NamespaceError::InvalidName { name, .. } => assert_eq!(name, ""),
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[test]
    fn bad_charset_rejected_with_value() {
        let err = Namespace::new("has space").unwrap_err();
        match err {
            NamespaceError::InvalidName { name, .. } => assert_eq!(name, "has space"),
            other => panic!("expected InvalidName, got {other:?}"),
        }
        assert!(Namespace::new("emoji😀").is_err());
        assert!(Namespace::new("comma,sep").is_err());
    }

    #[test]
    fn too_long_rejected() {
        let long = "a".repeat(MAX_NAMESPACE_LEN + 1);
        assert!(Namespace::new(&long).is_err());
        let ok = "a".repeat(MAX_NAMESPACE_LEN);
        assert!(Namespace::new(&ok).is_ok());
    }

    #[test]
    fn reserved_prefix_names_rejected() {
        // A namespace name may not use an engine-reserved prefix (symmetry with
        // the property-key reservation, Issue #3349 hardening).
        for name in ["__aletheia_ns", "__aletheia_foo", "__shred_x", "__shred_"] {
            let err = Namespace::new(name).unwrap_err();
            match err {
                NamespaceError::InvalidName { name: n, .. } => assert_eq!(n, name),
                other => panic!("expected InvalidName for {name}, got {other:?}"),
            }
        }
        // A name that merely *contains* (not starts with) the marker is fine,
        // as is a single-underscore near-miss.
        assert!(Namespace::new("agent:__aletheia").is_ok());
        assert!(Namespace::new("_aletheia_ns").is_ok());
    }

    #[test]
    fn all_selector_rejected() {
        assert!(matches!(
            Namespace::new("all"),
            Err(NamespaceError::InvalidName { .. })
        ));
        // Case-sensitive: "All" is a distinct, valid name.
        assert!(Namespace::new("All").is_ok());
    }

    #[test]
    fn default_is_default() {
        assert!(Namespace::default().is_default());
        assert_eq!(Namespace::default().as_str(), "default");
        assert!(Namespace::new("default").unwrap().is_default());
        assert!(!Namespace::new("agent:x").unwrap().is_default());
    }

    #[test]
    fn reserved_prefix_policy() {
        assert!(is_reserved_property_key("__aletheia_ns"));
        assert!(is_reserved_property_key("__aletheia_foo"));
        assert!(is_reserved_property_key("__shred_x"));
        assert!(!is_reserved_property_key("name"));
        assert!(!is_reserved_property_key("_aletheia_ns"));
        assert!(!is_reserved_property_key("aletheia_ns"));
    }

    #[test]
    fn namespace_key_is_reserved() {
        assert!(is_reserved_property_key(NAMESPACE_KEY));
    }

    #[test]
    fn stamp_and_read_round_trip() {
        let ns = Namespace::new("agent:planner").unwrap();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let stamped = stamp_namespace(props, &ns);
        assert_eq!(namespace_of(&stamped), ns);
        // The reserved key is present in raw storage form...
        assert!(stamped.contains_key(NAMESPACE_KEY));
        // ...but elided from the user-facing view.
        let view = user_facing_properties(&stamped);
        assert!(!view.contains_key(NAMESPACE_KEY));
        assert_eq!(
            view.get("name").and_then(PropertyValue::as_str),
            Some("Alice")
        );
    }

    #[test]
    fn default_namespace_not_stamped() {
        let props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let stamped = stamp_namespace(props.clone(), &Namespace::default());
        // No ride-along key: byte-identical to legacy data.
        assert!(!stamped.contains_key(NAMESPACE_KEY));
        assert_eq!(namespace_of(&stamped), Namespace::default());
    }

    #[test]
    fn missing_key_resolves_to_default() {
        let props = PropertyMapBuilder::new().insert("name", "Legacy").build();
        assert_eq!(namespace_of(&props), Namespace::default());
    }

    #[test]
    fn restamp_preserves_immutable_namespace() {
        let ns = Namespace::new("agent:planner").unwrap();
        let existing = stamp_namespace(
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            &ns,
        );
        // A full-overwrite map (no ns key) re-stamps back to the original ns.
        let overwrite = PropertyMapBuilder::new().insert("name", "Alice2").build();
        let restamped = restamp_namespace(overwrite, &existing);
        assert_eq!(namespace_of(&restamped), ns);
    }

    #[test]
    fn reject_reserved_keys_catches_forgery() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert(NAMESPACE_KEY, "forged")
            .build();
        assert!(matches!(
            reject_reserved_keys(&props),
            Err(NamespaceError::ReservedPropertyKey { .. })
        ));

        let shred = PropertyMapBuilder::new().insert("__shred_x", 1).build();
        assert!(reject_reserved_keys(&shred).is_err());

        let clean = PropertyMapBuilder::new().insert("name", "Alice").build();
        assert!(reject_reserved_keys(&clean).is_ok());
    }

    #[test]
    fn unresolved_sentinel_is_fail_closed() {
        // The sentinel is a reserved value users can never construct via `new`...
        assert!(Namespace::new(UNRESOLVED_NAMESPACE).is_err());
        // ...but the engine can mint it internally.
        let sentinel = Namespace::unresolved();
        assert_eq!(sentinel.as_str(), UNRESOLVED_NAMESPACE);
        assert!(!sentinel.is_default());

        // No user scope may match the sentinel (no leak to any user-scoped read /
        // subscriber): Single(default), Single(agent:x), and a List all exclude it.
        assert!(!NamespaceScope::Single(Namespace::default()).contains(&sentinel));
        assert!(!NamespaceScope::Single(Namespace::new("agent:x").unwrap()).contains(&sentinel));
        assert!(
            !NamespaceScope::list(vec![
                Namespace::new("agent:a").unwrap(),
                Namespace::new("agent:b").unwrap(),
            ])
            .unwrap()
            .contains(&sentinel)
        );
        // But `All` (and, at the filter layer, an unset scope) still sees it, so an
        // unscoped `list_changes` never silently loses a genuinely-unresolvable change.
        assert!(NamespaceScope::All.contains(&sentinel));

        // The sentinel round-trips through the interner (resolve must NOT drop a
        // reserved value to None / default).
        let id = unresolved_namespace_id();
        assert_eq!(resolve_namespace_id(id), Some(sentinel));
    }

    #[test]
    fn user_facing_properties_no_reserved_keys() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert(NAMESPACE_KEY, "agent:x")
            .build();
        let view = user_facing_properties(&props);
        for (key, _) in view.iter() {
            let name = GLOBAL_INTERNER
                .resolve_with(*key, |s| s.to_string())
                .unwrap();
            assert!(
                !is_reserved_property_key(&name),
                "reserved key {name} leaked into user-facing view"
            );
        }
    }

    // ------------------------------------------------------------------------
    // Scope reconciliation (Issue #3349): `same_scope` + `reconcile_namespace_scope`.
    // ------------------------------------------------------------------------

    fn ns(name: &str) -> Namespace {
        Namespace::new(name).unwrap()
    }

    fn single(name: &str) -> NamespaceScope {
        NamespaceScope::single(ns(name))
    }

    fn list(names: &[&str]) -> NamespaceScope {
        NamespaceScope::list(names.iter().map(|n| ns(n)).collect()).unwrap()
    }

    #[test]
    fn same_scope_all_equals_only_all() {
        assert!(NamespaceScope::All.same_scope(&NamespaceScope::All));
        assert!(!NamespaceScope::All.same_scope(&single("agent:a")));
        assert!(!single("agent:a").same_scope(&NamespaceScope::All));
        assert!(!NamespaceScope::All.same_scope(&list(&["agent:a", "agent:b"])));
    }

    #[test]
    fn same_scope_single_equals_singleton_list() {
        // `Single(x)` and `List([x])` both select exactly `{x}`.
        assert!(single("agent:a").same_scope(&list(&["agent:a"])));
        assert!(list(&["agent:a"]).same_scope(&single("agent:a")));
    }

    #[test]
    fn same_scope_list_is_order_and_duplicate_insensitive() {
        assert!(list(&["agent:a", "agent:b"]).same_scope(&list(&["agent:b", "agent:a"])));
        assert!(
            list(&["agent:a", "agent:a", "agent:b"]).same_scope(&list(&["agent:b", "agent:a"]))
        );
        // A strict subset is NOT the same set.
        assert!(!list(&["agent:a"]).same_scope(&list(&["agent:a", "agent:b"])));
    }

    #[test]
    fn same_scope_is_case_sensitive() {
        // Namespace equality is the codebase's exact, case-sensitive equality;
        // no normalization silently equates differing cases.
        assert!(!single("agent:a").same_scope(&single("Agent:A")));
    }

    #[test]
    fn reconcile_both_none_is_default() {
        assert_eq!(
            reconcile_namespace_scope(None, None).unwrap(),
            NamespaceScope::default()
        );
    }

    #[test]
    fn reconcile_none_plus_clause_honors_clause() {
        assert_eq!(
            reconcile_namespace_scope(None, Some(single("agent:a"))).unwrap(),
            single("agent:a")
        );
        assert_eq!(
            reconcile_namespace_scope(None, Some(NamespaceScope::All)).unwrap(),
            NamespaceScope::All
        );
    }

    #[test]
    fn reconcile_param_plus_none_is_param() {
        assert_eq!(
            reconcile_namespace_scope(Some(single("agent:a")), None).unwrap(),
            single("agent:a")
        );
    }

    #[test]
    fn reconcile_identical_param_and_clause_allowed() {
        assert_eq!(
            reconcile_namespace_scope(Some(single("agent:a")), Some(list(&["agent:a"]))).unwrap(),
            single("agent:a")
        );
        assert_eq!(
            reconcile_namespace_scope(
                Some(list(&["agent:a", "agent:b"])),
                Some(list(&["agent:b", "agent:a"])),
            )
            .unwrap(),
            list(&["agent:a", "agent:b"])
        );
    }

    #[test]
    fn reconcile_conflicting_param_and_clause_is_scope_conflict() {
        // Disjoint.
        assert_eq!(
            reconcile_namespace_scope(Some(single("agent:a")), Some(single("agent:b"))),
            Err(NamespaceError::ScopeConflict)
        );
        // Wider clause than the param ceiling.
        assert_eq!(
            reconcile_namespace_scope(Some(single("agent:a")), Some(NamespaceScope::All)),
            Err(NamespaceError::ScopeConflict)
        );
        // Narrower clause than an `All` param (still refused — we never compute
        // subset relations, to avoid any widen-by-accident leak).
        assert_eq!(
            reconcile_namespace_scope(Some(NamespaceScope::All), Some(single("agent:a"))),
            Err(NamespaceError::ScopeConflict)
        );
        // Strict-subset list.
        assert_eq!(
            reconcile_namespace_scope(
                Some(list(&["agent:a"])),
                Some(list(&["agent:a", "agent:b"]))
            ),
            Err(NamespaceError::ScopeConflict)
        );
    }
}
