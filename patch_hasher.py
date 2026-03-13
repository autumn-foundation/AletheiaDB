import re
with open("src/core/hasher.rs", "r") as f:
    content = f.read()

test_code = """
    #[test]
    fn test_identity_hasher_update_state_zero_and_dirty() {
        let mut hasher = IdentityHasher::default();
        hasher.update_state(42);
        assert_eq!(hasher.finish(), 42);

        hasher.update_state(10);
        let expected = (42u64 ^ 10u64).wrapping_mul(FNV_PRIME);
        assert_eq!(hasher.finish(), expected);

        hasher.update_state(7);
        let expected2 = (expected ^ 7u64).wrapping_mul(FNV_PRIME);
        assert_eq!(hasher.finish(), expected2);
    }
"""

content = content.replace("    #[test]\n    fn test_identity_hasher_update_state_mutants() {", test_code + "\n    #[test]\n    fn test_identity_hasher_update_state_mutants() {")

with open("src/core/hasher.rs", "w") as f:
    f.write(content)
