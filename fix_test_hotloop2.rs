use std::fs;

fn main() {
    let filepath = "src/index/vector/hnsw/mod.rs";
    let mut content = fs::read_to_string(filepath).unwrap();

    let old_fn_start = r#"pub(crate) fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{"#;

    let new_fn_start = r#"pub(crate) fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{
    // 🛡️ Havoc/Warden: Validate dims exactly ONCE at wrapper creation, not in the hot loop.
    // If dims are invalid, we can't safely create slices. We shouldn't panic, but we
    // should probably return a dummy function that just returns f32::MAX.
    if dims == 0 || dims > crate::core::vector::constants::MAX_VECTOR_DIMENSIONS {
        eprintln!(
            "Attempted to create metric wrapper with invalid dimensions (got {}, max {})",
            dims, crate::core::vector::constants::MAX_VECTOR_DIMENSIONS
        );
        return Box::new(|_, _| f32::MAX);
    }"#;

    if content.contains(old_fn_start) {
        content = content.replace(old_fn_start, new_fn_start);
        fs::write(filepath, content).unwrap();
    }
}
