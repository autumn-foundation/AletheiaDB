//! Query builder and execution interface.
//!
//! This module provides the fluent query API and the Cypher/AQL execution
//! methods for retrieving data from the graph.

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
    pub fn execute_aql(&self, query_string: &str) -> Result<QueryResults> {
        let query = crate::query::parse_query(query_string)?;
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
    pub fn execute_query(&self, query: Query) -> Result<QueryResults> {
        let result = (|| {
            #[cfg(feature = "observability")]
            let _span = tracing::info_span!("execute_query").entered();

            // Use cached statistics for cost-based optimization
            // Statistics are shared across all queries for this database instance
            let planner = QueryPlanner::new(Arc::clone(&self.stats), Arc::clone(&self.current));
            let physical_plan = planner.plan(query)?;

            // Execute the plan
            let executor =
                QueryExecutor::new(Arc::clone(&self.current), Arc::clone(&self.historical));

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
    pub fn traverse_and_rank(
        &self,
        source: NodeId,
        edge_label: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("traverse_and_rank").entered();

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
    pub fn find_similar_at_time(
        &self,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_at_time").entered();

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
        let _span = tracing::info_span!("traverse_and_rank_at_time").entered();

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
    pub fn execute_cypher(&self, query_string: &str) -> Result<QueryResults> {
        let query = crate::cypher::parse_cypher(query_string)?;
        self.execute_query(query)
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
    pub fn execute_cypher_with_params(
        &self,
        query_string: &str,
        params: std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
    ) -> Result<QueryResults> {
        let query = crate::cypher::parse_cypher_with_params(query_string, params)?;
        self.execute_query(query)
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
