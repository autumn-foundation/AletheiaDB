#![no_main]
use libfuzzer_sys::fuzz_target;
use aletheiadb::core::vector::ops::{cosine_similarity, cosine_similarity_normalized, normalize, dot_product};

fuzz_target!(|data: (Vec<f32>, Vec<f32>)| {
    let (mut a, mut b) = data;

    // Truncate sizes so they match
    let min_len = a.len().min(b.len());
    a.truncate(min_len);
    b.truncate(min_len);

    // Limit dimensions to what the system expects
    if a.len() > 1000 {
        a.truncate(1000);
        b.truncate(1000);
    }

    // We expect these to handle arbitrary floats without panicking
    let _ = dot_product(&a, &b);
    let _ = cosine_similarity(&a, &b);

    let a_norm = normalize(&a);
    let b_norm = normalize(&b);
    let _ = cosine_similarity_normalized(&a_norm, &b_norm);
});
