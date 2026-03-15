with open("src/experimental/temporal_narrative.rs", "r") as f:
    code = f.read()

import re

old_impl_regex = r'#\[cfg\(not\(feature = "nova"\)\)\]\n#\[allow\(deprecated\)\]\nimpl<\'a> NarrativeGenerator<\'a> \{.*?\}\n\}'

new_impl = """#[cfg(not(feature = "nova"))]
#[allow(deprecated)]
impl<'a> NarrativeGenerator<'a> {
    /// Create a new narrative generator.
    #[allow(unused_variables)]
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    /// Generate a narrative for a specific node.
    ///
    /// # Errors
    ///
    /// This method returns an `Error::NotImplemented` if the `nova` feature is not enabled.
    #[allow(unused_variables)]
    pub fn generate_node_narrative(&self, node_id: NodeId) -> Result<Vec<NarrativeEvent>> {
        Err(crate::core::error::Error::NotImplemented {
            feature: "NarrativeGenerator".to_string(),
            reason: "NarrativeGenerator requires the 'nova' feature. Add 'features = [\\"nova\\"]' to your Cargo.toml.".to_string(),
        })
    }
}"""

code = re.sub(old_impl_regex, new_impl, code, flags=re.DOTALL)

old_tests_regex = r'#\[cfg\(all\(test, not\(feature = "nova"\)\)\)\]\n#\[allow\(deprecated\)\]\nmod stub_tests \{.*'

new_tests = """#[cfg(all(test, not(feature = "nova")))]
#[allow(deprecated)]
mod stub_tests {
    use super::*;

    #[test]
    fn test_stub_no_panic_on_new() {
        let db = AletheiaDB::new().unwrap();
        // This should NOT panic
        let _ = NarrativeGenerator::new(&db);
    }

    #[test]
    fn test_stub_error_on_generate() {
        // Construct a fake NarrativeGenerator to test method error
        let generator: NarrativeGenerator<'_> = NarrativeGenerator {
            _marker: std::marker::PhantomData,
        };
        // This should return Err
        let result = generator.generate_node_narrative(NodeId::new(0).unwrap());
        assert!(result.is_err());

        if let Err(crate::core::error::Error::NotImplemented { feature, reason }) = result {
            assert_eq!(feature, "NarrativeGenerator");
            assert_eq!(reason, "NarrativeGenerator requires the 'nova' feature. Add 'features = [\\\"nova\\\"]' to your Cargo.toml.");
        } else {
            panic!("Expected NotImplemented error, got {:?}", result);
        }
    }
}"""

code = re.sub(old_tests_regex, new_tests, code, flags=re.DOTALL)

with open("src/experimental/temporal_narrative.rs", "w") as f:
    f.write(code)

print("Patch applied")
