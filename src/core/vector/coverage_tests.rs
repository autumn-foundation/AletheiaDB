#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sparse_squared_euclidean_distance_tail_coverage() {
        // Case 1: a has tail elements (covered by existing tests likely)
        // a: [2.0, 0.0] -> indices [0], values [2.0]
        // b: [0.0, 0.0] -> indices [], values []
        let a = SparseVec::new(vec![0], vec![2.0], 2).unwrap();
        let b = SparseVec::new(vec![], vec![], 2).unwrap();
        // dist^2 = (2-0)^2 + (0-0)^2 = 4.0
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 4.0);

        // Case 2: b has tail elements (lines 597-600)
        // a: [0.0, 0.0] -> indices [], values []
        // b: [0.0, 3.0] -> indices [1], values [3.0]
        let a = SparseVec::new(vec![], vec![], 2).unwrap();
        let b = SparseVec::new(vec![1], vec![3.0], 2).unwrap();
        // dist^2 = (0-0)^2 + (0-3)^2 = 9.0
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 9.0);

        // Case 3: Mixed overlap and tails
        // a: [1.0, 0.0, 2.0] -> indices [0, 2]
        // b: [1.0, 3.0, 0.0] -> indices [0, 1]
        // overlap at 0, b has tail at 1 relative to a's exhausted iterator?
        // No, merge sort logic handles interleaving.
        // We strictly want j < b.len() loop to run.
        // This happens when a is exhausted (i == a.len()) but j < b.len().

        // a: [1.0, 0.0] -> indices [0]
        // b: [0.0, 2.0] -> indices [1]
        // i=0 (idx 0), j=0 (idx 1). 0 < 1 -> process a[0], i++.
        // i=1 (done). j=0.
        // Loop terminates.
        // Remaining i loop: i=1 !< 1.
        // Remaining j loop: j=0 < 1. b[0] processed. j++.

        let a = SparseVec::new(vec![0], vec![1.0], 3).unwrap();
        let b = SparseVec::new(vec![1, 2], vec![2.0, 3.0], 3).unwrap();
        // dist^2 = 1^2 + 2^2 + 3^2 = 1 + 4 + 9 = 14
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 14.0);
    }
}
