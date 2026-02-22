use aletheiadb::core::vector::{normalize, normalize_in_place};

#[test]
fn test_normalize_vs_normalize_in_place_consistency() {
    // Vector with squared magnitude < 1e-14 (threshold)
    // 1e-8 squared is 1e-16.
    let small_val = 1e-8_f32;
    let v = vec![small_val];

    // normalize returns zero vector for small values
    let normalized = normalize(&v);
    assert_eq!(normalized, vec![0.0], "normalize should return zero vector for small inputs");

    // normalize_in_place should behave consistently?
    let mut v_in_place = v.clone();
    normalize_in_place(&mut v_in_place);

    // If they are consistent, v_in_place should be zeroed out.
    // Currently, it is likely unchanged (failed to normalize but didn't zero).
    assert_eq!(v_in_place, normalized,
        "normalize_in_place should match normalize behavior for small inputs. \
        Got {:?}, expected {:?}", v_in_place, normalized);
}
