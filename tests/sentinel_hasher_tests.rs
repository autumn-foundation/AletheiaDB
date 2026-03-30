use aletheiadb::core::hasher::IdentityHasher;
use std::hash::Hasher;

#[test]
fn test_identity_hasher_finish_return_mutants() {
    let mut h = IdentityHasher::default();
    h.write_u64(42);
    assert_eq!(h.finish(), 42);
    assert_ne!(h.finish(), 0);
    assert_ne!(h.finish(), 1);
}
