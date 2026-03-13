use aletheiadb::core::property::PropertyValue;

fn main() {
    let vec_with_nan: Vec<f32> = vec![1.0f32, f32::NAN, 2.0f32];
    let nan_vec_result = PropertyValue::try_vector(&vec_with_nan);
    println!("{:?}", nan_vec_result);
}
