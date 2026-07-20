//! Durable workflow-execution journal — DBOS Phase 3a.
//!
//! This module implements the *schema convention* and *exactly-once step
//! recording* primitive for durable workflow execution on top of AletheiaDB's
//! existing bi-temporal store. It deliberately does **not** implement the
//! executor loop, claims/leases, durable timers, wakeup, changefeed
//! subscriptions, or the safe-fencing primitive — those are Phases 3b/3c/3e.
//!
//! # Schema convention (§5.1)
//!
//! Two node tables plus one edge type model a workflow run and its steps:
//!
//! - **`WorkflowRun`** node — one per workflow instance, keyed by a UNIQUE
//!   `workflow_id`.
//! - **`Step`** node — one per (workflow, step-number), keyed by a UNIQUE
//!   `idem_key` = `"{workflow_id}:{step_number}"`.
//! - **`WorkflowRun -[:HAS_STEP]-> Step`** edge linking a run to each of its
//!   recorded steps.
//!
//! # Exactly-once recording (§5.2)
//!
//! A step is recorded by attempting to create the `Step` node (and its
//! `HAS_STEP` edge) inside a single atomic write transaction. The `idem_key`
//! UNIQUE constraint is the exactly-once anchor: a second attempt to record the
//! same `(workflow_id, step_number)` fails the commit with a constraint
//! violation, at which point we re-read the existing step and **adopt the
//! winner** — returning its recorded output rather than the loser's. Before
//! adopting, we assert the recorded step's `name` matches the caller's expected
//! name; a mismatch means two orchestrators disagree about what step N *is*, so
//! we fail loud with [`WorkflowError::OrchestrationDivergence`] (N6/§8 #12).
//!
//! # Why the library `db.write` transaction, not the MCP `apply_batch`
//!
//! The design describes recording "in one `apply_batch`", but `apply_batch` is
//! an MCP-surface tool (`AletheiaMcpServer`, `mcp-server` feature). A
//! library-level module must not depend on the MCP server. The equivalent —
//! and the primitive `apply_batch` itself composes over — is the
//! write-transaction closure [`AletheiaDB::write`], which commits all of its
//! operations all-or-nothing in one `WriteTransaction` (single WAL batch
//! append / group-commit fsync). We use it directly, so the `Step` node and its
//! `HAS_STEP` edge are committed atomically.

use thiserror::Error;

use crate::api::transaction::WriteOps;
use crate::core::error::ConstraintError;
use crate::core::graph::Node;
use crate::core::temporal::time;
use crate::{NodeId, PropertyMap, PropertyMapBuilder, PropertyValue, Timestamp};

use super::AletheiaDB;

/// Node label for a workflow-run record.
pub const LABEL_WORKFLOW_RUN: &str = "WorkflowRun";
/// Node label for a step record.
pub const LABEL_STEP: &str = "Step";
/// Edge label linking a run to each of its recorded steps.
pub const EDGE_HAS_STEP: &str = "HAS_STEP";

// --- WorkflowRun property keys ------------------------------------------------

/// `WorkflowRun.workflow_id` — the UNIQUE business key of a run.
pub const KEY_WORKFLOW_ID: &str = "workflow_id";
/// `WorkflowRun.name` — the workflow's (type) name.
pub const KEY_NAME: &str = "name";
/// `WorkflowRun.status` — pending / running / completed / failed.
pub const KEY_STATUS: &str = "status";
/// `WorkflowRun.owner` — the executor/agent that owns the run.
pub const KEY_OWNER: &str = "owner";
/// `WorkflowRun.lease_until` — lease expiry, micros since epoch.
pub const KEY_LEASE_UNTIL: &str = "lease_until";
/// `WorkflowRun.fence` — monotonic fence token (safe-fencing lands in 3e).
pub const KEY_FENCE: &str = "fence";
/// `WorkflowRun.input` — opaque serialized workflow input.
pub const KEY_INPUT: &str = "input";
/// `WorkflowRun.created_at` — creation time, micros since epoch.
pub const KEY_CREATED_AT: &str = "created_at";
/// `WorkflowRun.wake_at` — durable-timer wake time, micros since epoch.
pub const KEY_WAKE_AT: &str = "wake_at";

// --- Step property keys -------------------------------------------------------

/// `Step.step_number` — the step's ordinal within its workflow.
pub const KEY_STEP_NUMBER: &str = "step_number";
/// `Step.idem_key` — the UNIQUE idempotency key `"{workflow_id}:{step_number}"`.
pub const KEY_IDEM_KEY: &str = "idem_key";
/// `Step.output` — opaque serialized step output (present on completed steps).
pub const KEY_OUTPUT: &str = "output";
/// `Step.error` — failure message (present on failed steps).
pub const KEY_ERROR: &str = "error";

/// Build the canonical idempotency key for a `(workflow_id, step_number)` pair.
///
/// The format is `"{workflow_id}:{step_number}"` and is the value stored in the
/// UNIQUE `Step.idem_key` slot.
#[must_use]
pub fn idem_key(workflow_id: &str, step_number: i64) -> String {
    format!("{workflow_id}:{step_number}")
}

/// Lifecycle status of a [`WorkflowRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Created but not yet started.
    Pending,
    /// Actively executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

impl WorkflowStatus {
    /// The stable string token stored in `WorkflowRun.status`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            WorkflowStatus::Pending => "pending",
            WorkflowStatus::Running => "running",
            WorkflowStatus::Completed => "completed",
            WorkflowStatus::Failed => "failed",
        }
    }

    /// Parse a status token back into a [`WorkflowStatus`].
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(WorkflowStatus::Pending),
            "running" => Some(WorkflowStatus::Running),
            "completed" => Some(WorkflowStatus::Completed),
            "failed" => Some(WorkflowStatus::Failed),
            _ => None,
        }
    }
}

/// Terminal status of a recorded [`StepRecord`].
///
/// A step is only ever recorded once it has a terminal outcome, so unlike a
/// run it is either `Completed` or `Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The step completed and its `output` is authoritative.
    Completed,
    /// The step failed and its `error` describes why.
    Failed,
}

impl StepStatus {
    /// The stable string token stored in `Step.status`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Completed => "completed",
            StepStatus::Failed => "failed",
        }
    }

    /// Parse a status token back into a [`StepStatus`].
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "completed" => Some(StepStatus::Completed),
            "failed" => Some(StepStatus::Failed),
            _ => None,
        }
    }
}

/// Errors raised by the workflow journal.
///
/// This is a module-local error type; it is deliberately **not** folded into
/// the crate-wide [`crate::Error`]. Storage errors surfaced by the underlying
/// database are carried transparently via [`WorkflowError::Database`].
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// A run with the given `workflow_id` already exists (UNIQUE violation).
    #[error("workflow run '{workflow_id}' already exists")]
    RunAlreadyExists {
        /// The conflicting `workflow_id`.
        workflow_id: String,
    },

    /// The requested run does not exist.
    #[error("workflow run '{workflow_id}' not found")]
    RunNotFound {
        /// The `workflow_id` that was looked up.
        workflow_id: String,
    },

    /// Two orchestrators disagree about what a given step *is*: the recorded
    /// step's name does not match the caller's expected name. This is a
    /// fail-loud condition (N6 / §8 #12) — we never silently return a foreign
    /// step's output.
    #[error(
        "orchestration divergence on workflow '{workflow_id}' step {step_number}: \
         expected step named '{expected}', but the journal holds '{found}'"
    )]
    OrchestrationDivergence {
        /// The workflow whose step diverged.
        workflow_id: String,
        /// The step number that diverged.
        step_number: i64,
        /// The name the caller expected step N to have.
        expected: String,
        /// The name actually recorded for step N.
        found: String,
    },

    /// The step is recorded as failed; its error is propagated to the caller.
    #[error("workflow '{workflow_id}' step {step_number} previously failed: {message}")]
    StepFailed {
        /// The workflow that owns the failed step.
        workflow_id: String,
        /// The failed step's number.
        step_number: i64,
        /// The recorded failure message.
        message: String,
    },

    /// The user-supplied step executor returned an error. The step is **not**
    /// recorded, so a later retry re-executes it.
    #[error("execution of workflow '{workflow_id}' step {step_number} failed: {message}")]
    StepExecutionFailed {
        /// The workflow that owns the step.
        workflow_id: String,
        /// The step number that failed to execute.
        step_number: i64,
        /// The executor's error message.
        message: String,
    },

    /// A stored workflow/step record is missing a required property or holds an
    /// unexpected type. This indicates external corruption of the reserved
    /// schema, not normal operation.
    #[error("malformed workflow record: {0}")]
    Malformed(String),

    /// An error surfaced by the underlying database.
    #[error(transparent)]
    Database(#[from] crate::core::error::Error),
}

/// Error returned by a step executor closure passed to
/// [`WorkflowJournal::get_or_record_step`].
#[derive(Debug, Error)]
#[error("{message}")]
pub struct StepExecError {
    /// A human-readable description of the failure.
    pub message: String,
}

impl StepExecError {
    /// Construct a new executor error from any string-like message.
    pub fn new(message: impl Into<String>) -> Self {
        StepExecError {
            message: message.into(),
        }
    }
}

impl From<&str> for StepExecError {
    fn from(s: &str) -> Self {
        StepExecError::new(s)
    }
}

impl From<String> for StepExecError {
    fn from(s: String) -> Self {
        StepExecError { message: s }
    }
}

/// A typed view of a `WorkflowRun` node, parsed from its property map.
#[derive(Debug, Clone)]
pub struct WorkflowRun {
    node_id: NodeId,
    workflow_id: String,
    name: String,
    status: WorkflowStatus,
    owner: Option<String>,
    lease_until: Option<i64>,
    fence: Option<i64>,
    input: Option<Vec<u8>>,
    created_at: Option<i64>,
    wake_at: Option<i64>,
}

impl WorkflowRun {
    fn from_node(node: &Node) -> Result<Self, WorkflowError> {
        let props = &node.properties;
        let workflow_id = require_str(props, KEY_WORKFLOW_ID)?;
        let name = require_str(props, KEY_NAME)?;
        let status_token = require_str(props, KEY_STATUS)?;
        let status = WorkflowStatus::from_token(&status_token).ok_or_else(|| {
            WorkflowError::Malformed(format!("unknown WorkflowRun.status '{status_token}'"))
        })?;
        Ok(WorkflowRun {
            node_id: node.id,
            workflow_id,
            name,
            status,
            owner: opt_str(props, KEY_OWNER),
            lease_until: opt_int(props, KEY_LEASE_UNTIL),
            fence: opt_int(props, KEY_FENCE),
            input: opt_bytes(props, KEY_INPUT),
            created_at: opt_int(props, KEY_CREATED_AT),
            wake_at: opt_int(props, KEY_WAKE_AT),
        })
    }

    /// The node id backing this run (used as the `HAS_STEP` edge source).
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
    /// The UNIQUE business key of the run.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }
    /// The workflow's (type) name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The run's lifecycle status.
    #[must_use]
    pub fn status(&self) -> WorkflowStatus {
        self.status
    }
    /// The executor/agent owning the run, if set.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    /// Lease expiry (micros since epoch), if set.
    #[must_use]
    pub fn lease_until(&self) -> Option<i64> {
        self.lease_until
    }
    /// Monotonic fence token, if set.
    #[must_use]
    pub fn fence(&self) -> Option<i64> {
        self.fence
    }
    /// Opaque serialized workflow input, if set.
    #[must_use]
    pub fn input(&self) -> Option<&[u8]> {
        self.input.as_deref()
    }
    /// Creation time (micros since epoch), if set.
    #[must_use]
    pub fn created_at(&self) -> Option<i64> {
        self.created_at
    }
    /// Durable-timer wake time (micros since epoch), if set.
    #[must_use]
    pub fn wake_at(&self) -> Option<i64> {
        self.wake_at
    }
}

/// A typed view of a `Step` node, parsed from its property map.
#[derive(Debug, Clone)]
pub struct StepRecord {
    node_id: NodeId,
    workflow_id: String,
    step_number: i64,
    idem_key: String,
    name: String,
    status: StepStatus,
    output: Option<Vec<u8>>,
    error: Option<String>,
}

impl StepRecord {
    fn from_node(node: &Node) -> Result<Self, WorkflowError> {
        let props = &node.properties;
        let workflow_id = require_str(props, KEY_WORKFLOW_ID)?;
        let step_number = require_int(props, KEY_STEP_NUMBER)?;
        let idem_key = require_str(props, KEY_IDEM_KEY)?;
        let name = require_str(props, KEY_NAME)?;
        let status_token = require_str(props, KEY_STATUS)?;
        let status = StepStatus::from_token(&status_token).ok_or_else(|| {
            WorkflowError::Malformed(format!("unknown Step.status '{status_token}'"))
        })?;
        Ok(StepRecord {
            node_id: node.id,
            workflow_id,
            step_number,
            idem_key,
            name,
            status,
            output: opt_bytes(props, KEY_OUTPUT),
            error: opt_str(props, KEY_ERROR),
        })
    }

    /// The node id backing this step.
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
    /// The owning workflow's id.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }
    /// The step's ordinal within its workflow.
    #[must_use]
    pub fn step_number(&self) -> i64 {
        self.step_number
    }
    /// The UNIQUE idempotency key.
    #[must_use]
    pub fn idem_key(&self) -> &str {
        &self.idem_key
    }
    /// The step's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The step's terminal status.
    #[must_use]
    pub fn status(&self) -> StepStatus {
        self.status
    }
    /// The recorded output (present on completed steps).
    #[must_use]
    pub fn output(&self) -> Option<&[u8]> {
        self.output.as_deref()
    }
    /// The recorded failure message (present on failed steps).
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Specification for creating a new [`WorkflowRun`].
#[derive(Debug, Clone)]
pub struct CreateRunSpec {
    workflow_id: String,
    name: String,
    owner: Option<String>,
    input: Option<Vec<u8>>,
}

impl CreateRunSpec {
    /// Start a spec for a run with the given UNIQUE `workflow_id` and name.
    pub fn new(workflow_id: impl Into<String>, name: impl Into<String>) -> Self {
        CreateRunSpec {
            workflow_id: workflow_id.into(),
            name: name.into(),
            owner: None,
            input: None,
        }
    }

    /// Set the owning executor/agent.
    #[must_use]
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Set the opaque serialized workflow input.
    #[must_use]
    pub fn input(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.input = Some(input.into());
        self
    }
}

/// Specification for recording a single [`StepRecord`].
#[derive(Debug, Clone)]
pub struct StepRecordSpec {
    step_number: i64,
    name: String,
    status: StepStatus,
    output: Option<Vec<u8>>,
    error: Option<String>,
    valid_time: Option<Timestamp>,
}

impl StepRecordSpec {
    /// A spec for a **completed** step carrying `output`.
    pub fn completed(
        step_number: i64,
        name: impl Into<String>,
        output: impl Into<Vec<u8>>,
    ) -> Self {
        StepRecordSpec {
            step_number,
            name: name.into(),
            status: StepStatus::Completed,
            output: Some(output.into()),
            error: None,
            valid_time: None,
        }
    }

    /// A spec for a **failed** step carrying an error message.
    pub fn failed(step_number: i64, name: impl Into<String>, error: impl Into<String>) -> Self {
        StepRecordSpec {
            step_number,
            name: name.into(),
            status: StepStatus::Failed,
            output: None,
            error: Some(error.into()),
            valid_time: None,
        }
    }

    /// Record this step at a specific (possibly backdated) valid time.
    ///
    /// This affects only the bi-temporal `valid_from` coordinate of the written
    /// `Step` node / `HAS_STEP` edge; replay order is always by `step_number`,
    /// never by valid time.
    #[must_use]
    pub fn with_valid_time(mut self, valid_time: Timestamp) -> Self {
        self.valid_time = Some(valid_time);
        self
    }
}

/// The outcome of a [`WorkflowJournal::record_step`] call.
#[derive(Debug, Clone)]
pub struct StepOutcome {
    /// The authoritative recorded step (freshly written, or the adopted winner).
    pub record: StepRecord,
    /// `true` if an existing step was adopted after a constraint collision
    /// (idempotent replay); `false` if this call wrote the step.
    pub deduplicated: bool,
}

/// The memoized-or-executed value returned by
/// [`WorkflowJournal::get_or_record_step`].
#[derive(Debug, Clone)]
pub struct StepValue {
    /// The step's number.
    pub step_number: i64,
    /// The step's authoritative output bytes.
    pub output: Vec<u8>,
    /// `true` if the value came from the journal (no executor call); `false` if
    /// the executor ran on this call.
    pub from_memo: bool,
}

/// A handle for workflow-journal operations against an [`AletheiaDB`].
///
/// Obtain one via [`WorkflowJournalExt::workflow_journal`]. The handle borrows
/// the database and holds no state of its own.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowJournal<'a> {
    db: &'a AletheiaDB,
}

impl<'a> WorkflowJournal<'a> {
    /// Create a journal handle over the given database.
    #[must_use]
    pub fn new(db: &'a AletheiaDB) -> Self {
        WorkflowJournal { db }
    }

    /// Enable the two UNIQUE constraints the journal depends on:
    /// `(WorkflowRun, workflow_id)` and `(Step, idem_key)`.
    ///
    /// Idempotent: constraints already declared are skipped, so calling
    /// `bootstrap` repeatedly (e.g. on every process start) never errors.
    pub fn bootstrap(&self) -> Result<(), WorkflowError> {
        let existing = self.db.list_unique_constraints();
        let has = |label: &str, prop: &str| existing.iter().any(|(l, p)| l == label && p == prop);

        if !has(LABEL_WORKFLOW_RUN, KEY_WORKFLOW_ID) {
            self.db
                .unique_constraint(LABEL_WORKFLOW_RUN, KEY_WORKFLOW_ID)
                .enable()?;
        }
        if !has(LABEL_STEP, KEY_IDEM_KEY) {
            self.db
                .unique_constraint(LABEL_STEP, KEY_IDEM_KEY)
                .enable()?;
        }
        Ok(())
    }

    /// Create a new workflow run.
    ///
    /// Fails with [`WorkflowError::RunAlreadyExists`] if a run with the same
    /// `workflow_id` already exists (enforced by the UNIQUE constraint declared
    /// in [`bootstrap`](Self::bootstrap)).
    pub fn create_run(&self, spec: CreateRunSpec) -> Result<WorkflowRun, WorkflowError> {
        let created_at = time::now().wallclock();
        let mut builder = PropertyMapBuilder::new()
            .insert(KEY_WORKFLOW_ID, spec.workflow_id.as_str())
            .insert(KEY_NAME, spec.name.as_str())
            .insert(KEY_STATUS, WorkflowStatus::Pending.as_str())
            .insert(KEY_CREATED_AT, created_at);
        if let Some(owner) = spec.owner.as_deref() {
            builder = builder.insert(KEY_OWNER, owner);
        }
        if let Some(input) = spec.input.as_deref() {
            builder = builder.insert(KEY_INPUT, input);
        }
        let props = builder.build();

        let node_id = match self.db.create_node(LABEL_WORKFLOW_RUN, props) {
            Ok(id) => id,
            Err(e) => {
                if is_unique_violation(&e, KEY_WORKFLOW_ID) {
                    return Err(WorkflowError::RunAlreadyExists {
                        workflow_id: spec.workflow_id,
                    });
                }
                return Err(WorkflowError::Database(e));
            }
        };

        let node = self.db.get_node(node_id).map_err(WorkflowError::Database)?;
        WorkflowRun::from_node(&node)
    }

    /// Look up a run by its `workflow_id`, if present.
    pub fn get_run(&self, workflow_id: &str) -> Result<Option<WorkflowRun>, WorkflowError> {
        let value = PropertyValue::from(workflow_id);
        let ids = self
            .db
            .find_nodes_by_property(LABEL_WORKFLOW_RUN, KEY_WORKFLOW_ID, &value);
        match ids.first() {
            None => Ok(None),
            Some(&id) => {
                let node = self.db.get_node(id).map_err(WorkflowError::Database)?;
                Ok(Some(WorkflowRun::from_node(&node)?))
            }
        }
    }

    /// Record a single step, exactly once (§5.2).
    ///
    /// Attempts to atomically create the `Step` node and its `HAS_STEP` edge.
    /// If the `idem_key` UNIQUE constraint rejects the write (the step was
    /// already recorded), the existing step is re-read and **adopted** — but
    /// only after confirming its name matches `spec.name`; a mismatch is a
    /// fail-loud [`WorkflowError::OrchestrationDivergence`].
    pub fn record_step(
        &self,
        run: &WorkflowRun,
        spec: StepRecordSpec,
    ) -> Result<StepOutcome, WorkflowError> {
        let workflow_id = run.workflow_id().to_string();
        let key = idem_key(&workflow_id, spec.step_number);

        let mut builder = PropertyMapBuilder::new()
            .insert(KEY_WORKFLOW_ID, workflow_id.as_str())
            .insert(KEY_STEP_NUMBER, spec.step_number)
            .insert(KEY_IDEM_KEY, key.as_str())
            .insert(KEY_NAME, spec.name.as_str())
            .insert(KEY_STATUS, spec.status.as_str());
        if let Some(output) = spec.output.as_deref() {
            builder = builder.insert(KEY_OUTPUT, output);
        }
        if let Some(error) = spec.error.as_deref() {
            builder = builder.insert(KEY_ERROR, error);
        }
        let step_props = builder.build();

        let source = run.node_id();
        let valid_time = spec.valid_time;

        // Atomic all-or-nothing write: the Step node and the HAS_STEP edge
        // commit together in one WriteTransaction (see module docs). The
        // idem_key UNIQUE constraint is checked at commit; a collision surfaces
        // here as an Err.
        let write_result = self.db.write(|tx| {
            let step_id =
                tx.create_node_with_valid_time(LABEL_STEP, step_props.clone(), valid_time)?;
            tx.create_edge_with_valid_time(
                source,
                step_id,
                EDGE_HAS_STEP,
                PropertyMap::new(),
                valid_time,
            )?;
            Ok::<NodeId, crate::core::error::Error>(step_id)
        });

        match write_result {
            Ok(step_id) => {
                let node = self.db.get_node(step_id).map_err(WorkflowError::Database)?;
                let record = StepRecord::from_node(&node)?;
                Ok(StepOutcome {
                    record,
                    deduplicated: false,
                })
            }
            Err(e) if is_unique_violation(&e, KEY_IDEM_KEY) => {
                // Someone already recorded this step. Adopt the winner, after
                // asserting the recorded name matches what we intended to write.
                let existing = self
                    .get_step(&workflow_id, spec.step_number)?
                    .ok_or_else(|| {
                        WorkflowError::Malformed(format!(
                            "idem_key '{key}' collided but no Step could be re-read"
                        ))
                    })?;
                if existing.name() != spec.name {
                    return Err(WorkflowError::OrchestrationDivergence {
                        workflow_id,
                        step_number: spec.step_number,
                        expected: spec.name,
                        found: existing.name().to_string(),
                    });
                }
                Ok(StepOutcome {
                    record: existing,
                    deduplicated: true,
                })
            }
            Err(e) => Err(WorkflowError::Database(e)),
        }
    }

    /// Fetch a recorded step by `(workflow_id, step_number)`, if present.
    pub fn get_step(
        &self,
        workflow_id: &str,
        step_number: i64,
    ) -> Result<Option<StepRecord>, WorkflowError> {
        let key = idem_key(workflow_id, step_number);
        let value = PropertyValue::from(key.as_str());
        let ids = self
            .db
            .find_nodes_by_property(LABEL_STEP, KEY_IDEM_KEY, &value);
        match ids.first() {
            None => Ok(None),
            Some(&id) => {
                let node = self.db.get_node(id).map_err(WorkflowError::Database)?;
                Ok(Some(StepRecord::from_node(&node)?))
            }
        }
    }

    /// List all recorded steps for a workflow, sorted **ascending by
    /// `step_number`** (replay order — never by valid time or creation order).
    pub fn list_steps(&self, workflow_id: &str) -> Result<Vec<StepRecord>, WorkflowError> {
        let value = PropertyValue::from(workflow_id);
        let ids = self
            .db
            .find_nodes_by_property(LABEL_STEP, KEY_WORKFLOW_ID, &value);
        let mut steps = Vec::with_capacity(ids.len());
        for id in ids {
            let node = self.db.get_node(id).map_err(WorkflowError::Database)?;
            steps.push(StepRecord::from_node(&node)?);
        }
        steps.sort_by_key(StepRecord::step_number);
        Ok(steps)
    }

    /// Memoize-or-execute driver for a single step (§5.2, §8 #12).
    ///
    /// - If step `step_number` is already recorded: its name **must** equal
    ///   `expected_name` (else [`WorkflowError::OrchestrationDivergence`],
    ///   fail-loud, without calling `exec`). A completed step returns its
    ///   memoized output *without* running `exec`; a failed step propagates its
    ///   error via [`WorkflowError::StepFailed`].
    /// - Otherwise `exec` runs, and its output is recorded via
    ///   [`record_step`](Self::record_step) (which itself resolves a concurrent
    ///   record-race by adopting the winner). The recorded output is returned.
    pub fn get_or_record_step<F>(
        &self,
        run: &WorkflowRun,
        step_number: i64,
        expected_name: &str,
        exec: F,
    ) -> Result<StepValue, WorkflowError>
    where
        F: FnOnce() -> Result<Vec<u8>, StepExecError>,
    {
        let workflow_id = run.workflow_id().to_string();

        // Memo hit: return the recorded outcome without executing.
        if let Some(existing) = self.get_step(&workflow_id, step_number)? {
            if existing.name() != expected_name {
                return Err(WorkflowError::OrchestrationDivergence {
                    workflow_id,
                    step_number,
                    expected: expected_name.to_string(),
                    found: existing.name().to_string(),
                });
            }
            return match existing.status() {
                StepStatus::Completed => Ok(StepValue {
                    step_number,
                    output: existing.output().unwrap_or_default().to_vec(),
                    from_memo: true,
                }),
                StepStatus::Failed => Err(WorkflowError::StepFailed {
                    workflow_id,
                    step_number,
                    message: existing.error().unwrap_or_default().to_string(),
                }),
            };
        }

        // Memo miss: run the real work, then record it.
        let output = exec().map_err(|e| WorkflowError::StepExecutionFailed {
            workflow_id: workflow_id.clone(),
            step_number,
            message: e.message,
        })?;

        let spec = StepRecordSpec::completed(step_number, expected_name, output);
        let outcome = self.record_step(run, spec)?;
        Ok(StepValue {
            step_number,
            output: outcome.record.output().unwrap_or_default().to_vec(),
            from_memo: outcome.deduplicated,
        })
    }
}

/// Extension trait exposing the workflow journal on [`AletheiaDB`].
pub trait WorkflowJournalExt {
    /// Obtain a [`WorkflowJournal`] handle bound to this database.
    fn workflow_journal(&self) -> WorkflowJournal<'_>;
}

impl WorkflowJournalExt for AletheiaDB {
    fn workflow_journal(&self) -> WorkflowJournal<'_> {
        WorkflowJournal::new(self)
    }
}

// --- property helpers ---------------------------------------------------------

fn require_str(props: &PropertyMap, key: &str) -> Result<String, WorkflowError> {
    props
        .get(key)
        .and_then(PropertyValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| WorkflowError::Malformed(format!("missing or non-string property '{key}'")))
}

fn require_int(props: &PropertyMap, key: &str) -> Result<i64, WorkflowError> {
    props
        .get(key)
        .and_then(PropertyValue::as_int)
        .ok_or_else(|| WorkflowError::Malformed(format!("missing or non-integer property '{key}'")))
}

fn opt_str(props: &PropertyMap, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(PropertyValue::as_str)
        .map(str::to_string)
}

fn opt_int(props: &PropertyMap, key: &str) -> Option<i64> {
    props.get(key).and_then(PropertyValue::as_int)
}

fn opt_bytes(props: &PropertyMap, key: &str) -> Option<Vec<u8>> {
    props
        .get(key)
        .and_then(PropertyValue::as_bytes)
        .map(<[u8]>::to_vec)
}

/// Does `err` carry a UNIQUE-constraint violation on `property`?
fn is_unique_violation(err: &crate::core::error::Error, property: &str) -> bool {
    matches!(
        err.as_constraint(),
        Some(ConstraintError::UniqueViolation { property: p, .. }) if p == property
    )
}

// A tiny sanity check that the module builds and the id/enum helpers are
// self-consistent. Behavioural coverage lives in `tests/workflow_journal.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idem_key_format_is_workflow_colon_step() {
        assert_eq!(idem_key("wf-42", 7), "wf-42:7");
        assert_eq!(idem_key("a:b", 0), "a:b:0");
    }

    #[test]
    fn workflow_status_round_trips() {
        for s in [
            WorkflowStatus::Pending,
            WorkflowStatus::Running,
            WorkflowStatus::Completed,
            WorkflowStatus::Failed,
        ] {
            assert_eq!(WorkflowStatus::from_token(s.as_str()), Some(s));
        }
        assert_eq!(WorkflowStatus::from_token("bogus"), None);
    }

    #[test]
    fn step_status_round_trips() {
        for s in [StepStatus::Completed, StepStatus::Failed] {
            assert_eq!(StepStatus::from_token(s.as_str()), Some(s));
        }
        assert_eq!(StepStatus::from_token("pending"), None);
    }
}
