//! Query Optimization Rules
//!
//! Defines optimization rules that transform logical plans to more
//! efficient forms. Rules are applied iteratively until no more
//! transformations are possible.

use crate::query::plan::LogicalPlan;
use crate::core::error::Result;

use super::stats::Statistics;

mod filter_scan_fusion;
mod limit_pushdown;
mod operation_reordering;
mod predicate_pushdown;

pub use filter_scan_fusion::FilterScanFusion;
pub use limit_pushdown::LimitPushdown;
pub use operation_reordering::OperationReordering;
pub use predicate_pushdown::PredicatePushdown;

/// Trait for optimization rules.
///
/// Each rule attempts to transform a logical plan into a more efficient form.
/// Rules should return `Ok(Some(new_plan))` if a transformation was applied,
/// or `Ok(None)` if the rule doesn't apply.
pub trait OptimizationRule: Send + Sync {
    /// Name of this rule (for debugging/tracing)
    fn name(&self) -> &str;

    /// Attempt to apply this rule to a logical plan.
    ///
    /// Returns:
    /// - `Ok(Some(plan))` if the rule was applied
    /// - `Ok(None)` if the rule doesn't apply
    /// - `Err(e)` if an error occurred
    fn apply(&self, plan: &LogicalPlan, stats: &Statistics) -> Result<Option<LogicalPlan>>;
}

/// Create the default set of optimization rules.
#[must_use]
pub fn default_rules() -> Vec<Box<dyn OptimizationRule>> {
    vec![
        Box::new(PredicatePushdown),
        Box::new(FilterScanFusion),
        Box::new(LimitPushdown),
        Box::new(OperationReordering),
        // Future rules:
        // Box::new(VectorSearchReordering),
        // Box::new(TemporalBatching),
        // Box::new(IndexSelection),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_rules_exist() {
        let rules = default_rules();
        assert!(!rules.is_empty());

        // Check rule names
        let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
        assert!(names.contains(&"predicate-pushdown"));
        assert!(names.contains(&"filter-scan-fusion"));
        assert!(names.contains(&"limit-pushdown"));
        assert!(names.contains(&"operation-reordering"));
    }
}
