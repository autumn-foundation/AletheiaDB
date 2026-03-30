fn intersect_metadata_indices(
    a: &[u32],
    b: &[u32],
) -> Vec<u32> {
    const HASH_THRESHOLD: usize = 16;
    let max_len = a.len().max(b.len());

    if max_len < HASH_THRESHOLD {
        if a.len() <= b.len() {
            a.iter().copied().filter(|v| b.contains(v)).collect()
        } else {
            b.iter().copied().filter(|v| a.contains(v)).collect()
        }
    } else {
        use std::collections::HashSet;
        let b_set: HashSet<_> = b.iter().copied().collect();
        a.iter().copied().filter(|v| b_set.contains(v)).collect()
    }
}

fn intersect_metadata_indices_iter<'a>(
    a: impl Iterator<Item = u32> + 'a,
    b: impl Iterator<Item = u32> + 'a + Clone,
) -> impl Iterator<Item = u32> + 'a {
    // Cannot do this without cloning iterator or collecting.
    a.filter(move |v| b.clone().any(|x| x == *v))
}

fn main() {
    let a = vec![1, 2, 3];
    let b = vec![2, 3, 4];
    let mut results = Vec::new();
    for v in intersect_metadata_indices_iter(a.into_iter(), b.into_iter()) {
        results.push(v);
    }
    println!("{:?}", results);
}
