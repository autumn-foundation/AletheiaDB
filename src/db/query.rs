//! Query builder and execution entry points.
//!
//! Provides access to the query planner, AQL execution, and hybrid search methods.
use crate::core::error::{Result, ResultExt};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::query::builder::state::Initial;
use crate::query::{Query, QueryBuilder, QueryExecutor, QueryPlanner, QueryResults};
use std::sync::Arc;

impl AletheiaDB {
    /// Execute a Cypher-like AletheiaDB Query Language (AQL) string.
    ///
    /// This is a convenience method that parses the query string and executes it.
    ///
    /// # Arguments
    ///
    /// * `query_string` - The AQL query string to execute
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let results = db.execute_aql("MATCH (n:Person {name: 'Alice'}) RETURN n")?;
    /// for row in results {
    ///     println!("{:?}", row);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_aql(&self, query_string: &str) -> Result<QueryResults> {
        let query = crate::query::parse_query(query_string)?;
        self.execute_query(query)
    }

    /// Execute an AQL string scoped to a namespace read scope (Issue #3349, PR3d).
    ///
    /// The parsed query is stamped with `scope` before planning/execution, so it
    /// runs through exactly the same executor scope machinery as
    /// [`QueryBuilder::in_namespace`](crate::query::QueryBuilder::in_namespace):
    /// produced entities are filtered to those whose (immutable) namespace ∈
    /// `scope`, and graph traversal never crosses an out-of-scope edge nor
    /// bridges through an out-of-scope node.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if `scope` names an unregistered namespace,
    ///   `INVALID_ARGUMENT` for an empty `List` scope (both via
    ///   [`validate_scope`](Self::validate_scope), invoked by
    ///   [`execute_query`](Self::execute_query)).
    /// - The parse / execution errors of [`execute_aql`](Self::execute_aql).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_aql_scoped(
        &self,
        query_string: &str,
        scope: crate::core::namespace::NamespaceScope,
    ) -> Result<QueryResults> {
        let mut query = crate::query::parse_query(query_string)?;
        query.scope = Some(scope);
        self.execute_query(query)
    }

    /// Create a new query builder for constructing hybrid queries.
    ///
    /// This is the entry point for the fluent query API that enables
    /// combining graph traversal, vector search, and temporal queries.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let bob_embedding = vec![0.1, 0.2, 0.3];
    /// # let embedding = vec![0.1, 0.2, 0.3];
    /// # let timestamp_2023 = Timestamp::from(1672531200000000);
    /// # let tx_time = Timestamp::from(1672531200000000);
    /// // Graph + Vector: "Who does Alice know that's similar to Bob?"
    /// let query1 = db.query()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity(&bob_embedding, 10)
    ///     .build();
    ///
    /// let results1 = db.execute_query(query1)?;
    ///
    /// // Temporal + Vector: "What was similar in 2023?"
    /// let query2 = db.query()
    ///     .as_of(timestamp_2023, tx_time)
    ///     .find_similar(&embedding, 10)
    ///     .build();
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn query(&self) -> QueryBuilder<Initial> {
        QueryBuilder::new()
    }

    /// Execute a query and return the results.
    ///
    /// This method plans and executes the query using the hybrid query planner.
    /// The planner applies optimization rules and chooses the best execution
    /// strategy based on cost estimation.
    ///
    /// # Arguments
    ///
    /// * `query` - The query to execute
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let embedding = vec![0.1, 0.2, 0.3];
    /// let query = db.query()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity(&embedding, 10)
    ///     .build();
    ///
    /// let results = db.execute_query(query)?;
    /// for row in results {
    ///     println!("{:?}", row);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_query(&self, query: Query) -> Result<QueryResults> {
        let result = (|| {
            #[cfg(feature = "observability")]
            let _span =
                crate::observability::query_execute_span("query.execute", "hybrid").entered();

            // Namespace scope (Issue #3349, PR2) is orthogonal to planning: the
            // planner ignores it and the executor applies it as an entity filter
            // (and traversal boundary). Lift it out before the plan consumes the
            // query by value.
            let scope = query.scope.clone();

            // Validate the scope before planning/executing (Issue #3349 A3):
            // an unknown namespace is a `NOT_FOUND` (not a silently empty
            // result), and an empty `List` scope built via
            // `QueryBuilder::in_namespaces([])` is an `INVALID_ARGUMENT` (not a
            // silent unscoped-all). `All`/omitted scope validate trivially.
            if let Some(scope) = &scope {
                self.validate_scope(scope)?;
            }

            // Use cached statistics for cost-based optimization
            // Statistics are shared across all queries for this database instance
            let planner = QueryPlanner::new(Arc::clone(&self.stats), Arc::clone(&self.current));
            let physical_plan = planner.plan(query)?;

            // Execute the plan
            let mut executor =
                QueryExecutor::new(Arc::clone(&self.current), Arc::clone(&self.historical));
            if let Some(scope) = scope {
                executor = executor.with_namespace_scope(scope);
            }

            executor.execute(physical_plan)
        })();
        result.record_error_metric()
    }

    /// Traverse from a node and rank results by similarity to an embedding.
    ///
    /// This is a convenience method for a common hybrid query pattern:
    /// "Find nodes connected to X that are similar to Y."
    ///
    /// # Arguments
    ///
    /// * `source` - The starting node for traversal
    /// * `edge_label` - Edge type to traverse (e.g., "KNOWS")
    /// * `embedding` - Target embedding to rank by similarity
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let bob_embedding = vec![0.1, 0.2, 0.3];
    /// // "Who does Alice know that's similar to Bob?"
    /// let results = db.traverse_and_rank(
    ///     alice_id,
    ///     "KNOWS",
    ///     &bob_embedding,
    ///     10
    /// )?;
    ///
    /// for row in results {
    ///     println!("Found: {:?}", row?.entity.id());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn traverse_and_rank(
        &self,
        source: NodeId,
        edge_label: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::hybrid_query_span("traverse_and_rank").entered();

        let query = self
            .query()
            .start(source)
            .traverse(edge_label)
            .rank_by_similarity(embedding, k)
            .build();

        self.execute_query(query)
    }

    /// Find similar nodes at a specific point in time.
    ///
    /// This is a convenience method for temporal vector queries:
    /// "What was similar to this embedding at time T?"
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query embedding
    /// * `k` - Maximum number of results
    /// * `valid_time` - Valid time for the query
    /// * `transaction_time` - Transaction time for the query
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let query_embedding = vec![0.1, 0.2, 0.3];
    /// # let timestamp_2023 = Timestamp::from(1672531200000000);
    /// // "What concepts were similar to this in 2023?"
    /// let results = db.find_similar_at_time(
    ///     &query_embedding,
    ///     10,
    ///     timestamp_2023,
    ///     timestamp_2023
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_similar_at_time(
        &self,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::hybrid_query_span("find_similar_at_time").entered();

        let query = self
            .query()
            .as_of(valid_time, transaction_time)
            .find_similar(embedding, k)
            .build();

        self.execute_query(query)
    }

    /// Execute a full hybrid query combining graph, vector, and temporal.
    ///
    /// This is a convenience method for the most complex query pattern:
    /// "Who did X know at time T that was similar to Y?"
    ///
    /// # Arguments
    ///
    /// * `source` - Starting node for traversal
    /// * `edge_label` - Edge type to traverse
    /// * `embedding` - Target embedding to rank by similarity
    /// * `k` - Maximum number of results
    /// * `valid_time` - Valid time for the query
    /// * `transaction_time` - Transaction time for the query
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let bob_embedding = vec![0.1, 0.2, 0.3];
    /// # let timestamp_2023 = Timestamp::from(1672531200000000);
    /// // "Who did Alice know in 2023 that was similar to Bob?"
    /// let results = db.traverse_and_rank_at_time(
    ///     alice_id,
    ///     "KNOWS",
    ///     &bob_embedding,
    ///     10,
    ///     timestamp_2023,
    ///     timestamp_2023
    /// )?;
    /// for row in results {
    ///     println!("Found: {:?}", row?.entity.id());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn traverse_and_rank_at_time(
        &self,
        source: NodeId,
        edge_label: &str,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::hybrid_query_span("traverse_and_rank_at_time").entered();

        let query = self
            .query()
            .as_of(valid_time, transaction_time)
            .start(source)
            .traverse(edge_label)
            .rank_by_similarity(embedding, k)
            .build();

        self.execute_query(query)
    }
}

// ---------------------------------------------------------------------------
// Cypher query execution (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "cypher")]
impl AletheiaDB {
    /// Execute a Cypher query string.
    ///
    /// Parses the Cypher query into AletheiaDB's internal query IR and
    /// executes it through the standard query pipeline.
    ///
    /// # Arguments
    ///
    /// * `query_string` - A Cypher query string (e.g., `MATCH (n:Person) RETURN n`)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let results = db.execute_cypher("MATCH (n:Person {name: 'Alice'}) RETURN n")?;
    /// for row in results {
    ///     println!("{:?}", row);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_cypher(&self, query_string: &str) -> Result<QueryResults> {
        // A standalone `UNWIND` (Issue #559) expands a list into scalar rows
        // that have no `Query`-IR representation, so it is planned into a
        // pre-computed result stream; every other statement lowers to a `Query`
        // executed through the standard pipeline.
        match crate::cypher::plan_cypher(query_string)? {
            crate::cypher::CypherExecution::Query(query) => self.execute_query(query),
            crate::cypher::CypherExecution::Rows(results) => Ok(results),
            crate::cypher::CypherExecution::MultiPattern { statement, params } => {
                self.execute_multi_pattern(&statement, &params)
            }
            crate::cypher::CypherExecution::Explain(query) => self.explain_cypher_query(query),
            crate::cypher::CypherExecution::Profile(query) => self.profile_cypher_query(query),
            crate::cypher::CypherExecution::Mutation { statement, params } => {
                self.execute_mutation(&statement, &params)
            }
        }
    }

    /// Execute a Cypher query string with parameter bindings.
    ///
    /// Parameters are bound to `$param` references in the Cypher query,
    /// preventing injection attacks and enabling query reuse.
    ///
    /// # Arguments
    ///
    /// * `query_string` - A Cypher query string with `$param` references
    /// * `params` - A map of parameter names to values
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// use std::collections::HashMap;
    /// use aletheiadb::cypher::CypherParameterValue;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let mut params = HashMap::new();
    /// params.insert("name".to_string(), CypherParameterValue::String("Alice".into()));
    ///
    /// let results = db.execute_cypher_with_params(
    ///     "MATCH (n:Person {name: $name}) RETURN n",
    ///     params,
    /// )?;
    /// for row in results {
    ///     println!("{:?}", row);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_cypher_with_params(
        &self,
        query_string: &str,
        params: std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
    ) -> Result<QueryResults> {
        match crate::cypher::plan_cypher_with_params(query_string, params)? {
            crate::cypher::CypherExecution::Query(query) => self.execute_query(query),
            crate::cypher::CypherExecution::Rows(results) => Ok(results),
            crate::cypher::CypherExecution::MultiPattern { statement, params } => {
                self.execute_multi_pattern(&statement, &params)
            }
            crate::cypher::CypherExecution::Explain(query) => self.explain_cypher_query(query),
            crate::cypher::CypherExecution::Profile(query) => self.profile_cypher_query(query),
            crate::cypher::CypherExecution::Mutation { statement, params } => {
                self.execute_mutation(&statement, &params)
            }
        }
    }

    /// Execute a Cypher string scoped to a namespace read scope (Issue #3349,
    /// PR3d). See [`execute_aql_scoped`](Self::execute_aql_scoped) for the
    /// filtering/boundary semantics.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` for an invalid `scope`.
    /// - `UnsupportedFeature` when a **restricting** (non-`All`) scope is
    ///   combined with an execution shape the v1 scope machinery does not thread
    ///   through (the multi-variable-binding evaluator, or a mutation) — a
    ///   structured error, never a silently-unscoped result (design §6).
    /// - The parse / execution errors of [`execute_cypher`](Self::execute_cypher).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_cypher_scoped(
        &self,
        query_string: &str,
        scope: crate::core::namespace::NamespaceScope,
    ) -> Result<QueryResults> {
        self.execute_cypher_execution_scoped(crate::cypher::plan_cypher(query_string)?, scope)
    }

    /// Execute a parameterized Cypher string scoped to a namespace read scope
    /// (Issue #3349, PR3d). See [`execute_cypher_scoped`](Self::execute_cypher_scoped).
    ///
    /// # Errors
    ///
    /// As [`execute_cypher_scoped`](Self::execute_cypher_scoped).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn execute_cypher_with_params_scoped(
        &self,
        query_string: &str,
        params: std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
        scope: crate::core::namespace::NamespaceScope,
    ) -> Result<QueryResults> {
        self.execute_cypher_execution_scoped(
            crate::cypher::plan_cypher_with_params(query_string, params)?,
            scope,
        )
    }

    /// Apply a namespace read scope to a planned [`CypherExecution`] and run it
    /// (Issue #3349, PR3d).
    ///
    /// The single-entity `Query` pipeline (and `EXPLAIN`/`PROFILE` over it)
    /// carries the scope on the `Query` IR, so the standard executor applies the
    /// same source-leaf filter + traversal boundary as the Rust builder. A
    /// standalone `UNWIND` (`Rows`) yields scalar rows with no graph entities, so
    /// the scope only needs validating (unknown ns ⇒ `NOT_FOUND`) — there is
    /// nothing to filter. The multi-variable-binding evaluator and mutation paths
    /// do not thread scope in v1: under a **restricting** scope they return a
    /// structured `UnsupportedFeature` (never silently unscoped); under `All`
    /// (the no-op selector) they run unchanged.
    fn execute_cypher_execution_scoped(
        &self,
        execution: crate::cypher::CypherExecution,
        scope: crate::core::namespace::NamespaceScope,
    ) -> Result<QueryResults> {
        use crate::core::namespace::NamespaceScope;
        use crate::cypher::CypherExecution;
        let restricting = !matches!(scope, NamespaceScope::All);
        match execution {
            CypherExecution::Query(mut query) => {
                query.scope = Some(scope);
                self.execute_query(query)
            }
            CypherExecution::Explain(mut query) => {
                query.scope = Some(scope);
                self.explain_cypher_query(query)
            }
            CypherExecution::Profile(mut query) => {
                query.scope = Some(scope);
                self.profile_cypher_query(query)
            }
            CypherExecution::Rows(results) => {
                // Scalar UNWIND rows carry no graph entities; validate the scope
                // (an unknown ns is still `NOT_FOUND`, never silently ignored)
                // then return the rows unchanged.
                self.validate_scope(&scope)?;
                Ok(results)
            }
            CypherExecution::MultiPattern { statement, params } => {
                if restricting {
                    return Err(crate::cypher::CypherError::UnsupportedFeature(
                        "namespace scoping is not supported by the multi-variable pattern \
                         evaluator in v1; scope a single-variable MATCH, or pass \"all\" to run \
                         unscoped"
                            .to_string(),
                    )
                    .into());
                }
                self.execute_multi_pattern(&statement, &params)
            }
            CypherExecution::Mutation { statement, params } => {
                if restricting {
                    return Err(crate::cypher::CypherError::UnsupportedFeature(
                        "a namespace read scope cannot be applied to a mutating Cypher statement"
                            .to_string(),
                    )
                    .into());
                }
                self.execute_mutation(&statement, &params)
            }
        }
    }

    /// Execute a Cypher write statement (Issue #560): `CREATE` / `SET` /
    /// `DELETE` / `DETACH DELETE`.
    ///
    /// Dispatched here (rather than through the read-only `Query` pipeline)
    /// because mutations are applied against the native write APIs so each
    /// records the correct bi-temporal version. Reached only via
    /// [`Self::execute_cypher`] / [`Self::execute_cypher_with_params`]; the MCP
    /// `query` tool rejects mutating clauses before the parser runs
    /// (`crate::query::read_only::detect_mutating_clause`).
    fn execute_mutation(
        &self,
        statement: &crate::cypher::ast::CypherStatement,
        params: &std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
    ) -> Result<QueryResults> {
        crate::cypher::mutation::execute(self, statement, params)
    }

    /// Execute a multi-variable, multi-pattern `MATCH` (Issue #549).
    ///
    /// Delegates to the dedicated [`crate::cypher::multi_pattern`] evaluator,
    /// which binds several named variables per row (carried on
    /// [`QueryRow::bindings`](crate::query::executor::QueryRow::bindings)) --
    /// something the single-entity `Query` pipeline cannot represent. Reached
    /// only for statements the router
    /// ([`crate::cypher::exec::needs_multi_binding`]) classifies as
    /// multi-variable.
    fn execute_multi_pattern(
        &self,
        statement: &crate::cypher::ast::CypherStatement,
        params: &std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
    ) -> Result<QueryResults> {
        Ok(crate::cypher::multi_pattern::evaluate(
            self, statement, params,
        )?)
    }

    /// Plan (but do not execute) a Cypher `EXPLAIN` query, returning the
    /// physical plan as a single `plan` text row (Issue #562).
    ///
    /// This mirrors [`Self::execute_query`]'s planner construction but stops
    /// after planning -- no executor is run, so there are no side effects and
    /// the plan is returned even against an empty database.
    fn explain_cypher_query(&self, query: Query) -> Result<QueryResults> {
        let planner = QueryPlanner::new(Arc::clone(&self.stats), Arc::clone(&self.current));
        let physical_plan = planner.plan(query)?;
        Ok(Self::plan_text_result("plan", physical_plan.explain()))
    }

    /// Execute a Cypher `PROFILE` query with per-operator instrumentation,
    /// returning the plan annotated with executed row counts and timing as a
    /// single `plan` text row (Issue #562).
    ///
    /// The instrumented stream is drained (its data rows discarded -- `PROFILE`
    /// reports statistics, not the query's data, in v1) so the per-operator
    /// counters are fully populated before the annotated plan is rendered.
    fn profile_cypher_query(&self, query: Query) -> Result<QueryResults> {
        let planner = QueryPlanner::new(Arc::clone(&self.stats), Arc::clone(&self.current));
        let physical_plan = planner.plan(query)?;

        let executor = QueryExecutor::new(Arc::clone(&self.current), Arc::clone(&self.historical));
        let (results, registry) = executor.execute_profiled(&physical_plan)?;

        // Fully drain so every operator's counters are populated before render.
        let _ = results.collect_all()?;

        let annotations: Vec<String> = registry.iter().map(|op| op.annotation()).collect();
        Ok(Self::plan_text_result(
            "plan",
            physical_plan.explain_annotated(&annotations),
        ))
    }

    /// Wrap a rendered plan string in a single computed-column result row.
    fn plan_text_result(column: &str, text: String) -> QueryResults {
        let row = crate::query::executor::QueryRow::from_columns(vec![(
            column.to_string(),
            crate::core::property::PropertyValue::String(std::sync::Arc::from(text.as_str())),
        )]);
        QueryResults::from_rows(vec![row])
    }
}

#[cfg(test)]
mod tests_aql {

    use crate::AletheiaDB;
    use crate::core::property::PropertyMap;

    #[test]
    fn test_execute_aql_success() {
        let db = AletheiaDB::new().unwrap();
        let _n1 = db.create_node("TestLabel", PropertyMap::new()).unwrap();

        let results = db.execute_aql("MATCH (n:TestLabel) RETURN n").unwrap();
        let mut count = 0;
        for _row in results {
            count += 1;
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn test_execute_aql_parse_error() {
        let db = AletheiaDB::new().unwrap();
        let err = db.execute_aql("INVALID SYNTAX");
        assert!(err.is_err());
    }
}
