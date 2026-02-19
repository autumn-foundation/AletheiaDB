fn main() {
    let a = vec![-8.161245e-22f32];
    let b = vec![-125.53673f32];

    // Simulate dot_and_magnitudes logic
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a_sq: f32 = a.iter().map(|x| x * x).sum();
    let mag_b_sq: f32 = b.iter().map(|x| x * x).sum();

    let magnitude = (mag_a_sq * mag_b_sq).sqrt();

    let result = dot / magnitude;

    println!("a: {:?}", a);
    println!("b: {:?}", b);
    println!("dot: {:e}", dot);
    println!("mag_a_sq: {:e}", mag_a_sq);
    println!("mag_b_sq: {:e}", mag_b_sq);
    println!("magnitude: {:e}", magnitude);
    println!("result: {}", result);

    if result.abs() > 1.0 + 1e-3 {
        println!("PANIC WOULD OCCUR: {} > 1.00001", result);
    } else {
        println!("No panic.");
    }
}
