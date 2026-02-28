# Core Review Findings: `src/experimental/omen.rs`

## Findings

1. **Severity: high**
   - **File reference:** `src/experimental/omen.rs:136`
   - **What can break (concrete scenario):** A zero-duration window will result in a division by zero (`duration_secs == 0.0`), leading to NaN values in the velocity vector calculation.
   - **Why it breaks (technical reasoning):** In `calculate_trajectory`, there is a check for `duration_micros == 0`. However, if `duration_micros` is less than 1,000,000 but non-zero (e.g., 500 microseconds), `duration_secs` will be computed correctly as a small float (e.g., 0.0005). If the window duration is explicitly created to be zero, `duration_micros == 0` check handles it. BUT wait! If `duration_secs` is very small, we might get INF in velocity due to division. However, division by `duration_secs` is safe as long as it's not 0.0. The actual issue is the division in physics math:
     - `let v_dot_v: f32 = rel_vel.iter().map(|v| v * v).sum();`
     - `let t_secs = -p_dot_v / v_dot_v;`
     - If `v_dot_v` is close to 0, it falls into the `if v_dot_v < 1e-9` block and returns early. So that's safe.
     - Let's look closer at `duration_micros` check:
     ```rust
     let duration_micros = window.duration_micros().unwrap_or(0);
     if duration_micros == 0 {
         // Instantaneous window, velocity is zero? Or undefined?
         // Treat as zero velocity.
         let zero_vel = vec![0.0; start.len()];
         return Ok(Some((end, zero_vel)));
     }
     let duration_secs = duration_micros as f32 / 1_000_000.0;
     ```
     This logic is safe from exact divide-by-zero. What if `duration_micros` is very small? E.g., 1 microsecond. `duration_secs` = 1e-6. Velocity components become huge. This could lead to overflow/infinity in the velocity vector, and subsequent NaNs in `v_dot_v`.
   - **Minimal fix:** Cap `duration_secs` to a minimum value (e.g., `1e-6`) or handle `duration_micros` more robustly, or check if any element of `velocity` becomes Infinity/NaN. Let's look at another potential issue.

2. **Severity: high**
   - **File reference:** `src/experimental/omen.rs:107` and `110`
   - **What can break (concrete scenario):** `pos_a` and `pos_b` might have different lengths. For example, if `traj_a` returns a vector of length 2 and `traj_b` returns a vector of length 10. `zip` will silently truncate the longer vector to the length of the shorter one.
   - **Why it breaks (technical reasoning):** In `predict_encounter`, `rel_pos` and `rel_vel` are calculated using `zip` between vectors from `node_a` and `node_b`. If `pos_a.len() != pos_b.len()`, the `zip` iterator takes the minimum length. This silently ignores the extra dimensions, leading to a meaningless mathematical calculation of `distance` and `encounter` instead of returning an error or `None`.
   - **Minimal fix:** Add a dimension check before math:
     ```rust
     if pos_a.len() != pos_b.len() {
         return Ok(None);
     }
     ```
   - **Required tests:** A test case where `node_a` has a 2D vector and `node_b` has a 3D vector.

3. **Severity: medium**
   - **File reference:** `src/experimental/omen.rs:136`
   - **What can break:** Vector dimension mismatch between start and end versions.
   - **Why it breaks:** The code has `if start.len() != end.len() { return Ok(None); }`, which is correct.

4. **Severity: high**
   - **File reference:** `src/experimental/omen.rs:209`
   - **What can break (concrete scenario):** `best_vec` doesn't get updated properly if multiple versions have the *exact same* `valid_time.start`.
   - **Why it breaks (technical reasoning):** `vt_start >= best_time`. If two versions have the same start time, it depends on the iteration order. In temporal databases, transaction time or version ID usually breaks ties for the "latest" version at a valid time. Currently, it just overwrites with whatever comes later in the `history.versions` array. Since history versions are sorted by version ID (as per comment: "History is typically sorted by version ID"), `vt_start >= best_time` will correctly keep the latest version ID for that `vt_start`. So this is actually fine.

Let's refine finding #1: `pos_a.len() != pos_b.len()` is a silent mathematical error (data loss/corruption of the result).

Let's refine finding #2: What happens if `v_dot_v` is `NaN` or `Infinity`? `v_dot_v < 1e-9` will be `false` for `NaN` and `false` for `Infinity`. Then `t_secs = -p_dot_v / v_dot_v` might result in `NaN`. `pos_at_t.iter().map(|x| x * x).sum::<f32>().sqrt()` will be `NaN`. The result will be `NaN` distance. We should check if vectors contain `NaN` or if `pos_a.len() != pos_b.len()`.

Let's focus on the dimension mismatch, as it's a clear, concrete logical bug that silent truncation is dangerous in vector math.

---

### Chosen Findings

1. **Severity: high**
   - **File reference:** `src/experimental/omen.rs:107`
   - **What can break (concrete scenario):** Silent truncation of vectors during trajectory comparison if `node_a` and `node_b` have vectors of different dimensions. The function will return a nonsensical prediction based on a subset of dimensions rather than failing or returning `None`.
   - **Why it breaks (technical reasoning):** `pos_b.iter().zip(pos_a.iter())` stops at the length of the shorter iterator. If `pos_a` is 100D and `pos_b` is 50D, it calculates the relative position and velocity only for the first 50 dimensions, ignoring the rest, and calculates a false closest approach.
   - **Minimal fix:** Add `if pos_a.len() != pos_b.len() { return Ok(None); }` right after unpacking `pos_a` and `pos_b`. (Implemented)
   - **Required tests:** Add `test_omen_dimension_mismatch` ensuring it returns `Ok(None)` when calculating an encounter between nodes with mismatched vector dimensions. (Implemented)

## Test Gaps
- Missing tests for extremely short (but non-zero) windows causing potential velocity infinity/NaN.

## Minimal Patch Plan
1. Add length check `if pos_a.len() != pos_b.len() { return Ok(None); }` in `predict_encounter`. (Implemented)
2. Add `test_omen_dimension_mismatch` in `tests` module. (Implemented)