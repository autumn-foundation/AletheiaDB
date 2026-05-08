#[cfg(test)]
mod tests {
    use super::super::segment_reader::parse_entry_at;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn fuzz_parse_entry_at(data in prop::collection::vec(any::<u8>(), 0..100)) {
            let _ = parse_entry_at(&data, 0, 1);
        }
    }
}
