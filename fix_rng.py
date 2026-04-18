import re

with open("src/experimental/glitch.rs", "r") as f:
    content = f.read()

# Currently, `glitch_node_with_rng` exists, but the original implementation still contains `let mut rng = rand::thread_rng();` and `rng.r#gen...`
# We need to replace the `rng` usage to be the `rng` passed to `glitch_node_with_rng`.
# The current issue is:
# 64 | ...operty, config, &mut rng)
#    |                         ^^^ not found in this scope
# Let's completely rewrite the glitch_node and glitch_node_with_rng.

# Find the entire GlitchEngine impl block and replace the methods
start = content.find("impl<'a> GlitchEngine<'a> {")
end = content.find("#[cfg(all(test, feature = \"nova\"))]")

if start != -1 and end != -1:
    impl_block = """impl<'a> GlitchEngine<'a> {
    /// Create a new Glitch Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Apply a glitch to a specific node's vector property.
    pub fn glitch_node(
        &self,
        node_id: NodeId,
        property: &str,
        config: &GlitchConfig,
    ) -> Result<()> {
        let mut rng = rand::thread_rng();
        self.glitch_node_with_rng(node_id, property, config, &mut rng)
    }

    /// Apply a glitch to a specific node's vector property using a provided RNG.
    pub fn glitch_node_with_rng<R: rand::Rng>(
        &self,
        node_id: NodeId,
        property: &str,
        config: &GlitchConfig,
        rng: &mut R,
    ) -> Result<()> {
        let node = self.db.get_node(node_id)?;
        let mut vec = match node
            .properties
            .get(property)
            .and_then(|p| p.as_vector())
        {
            Some(v) => v.to_vec(),
            None => {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Node {} does not have vector property '{}'",
                    node_id, property
                ))));
            }
        };

        for v in &mut vec {
            // 1. Inversion
            if config.invert {
                *v = -*v;
            }

            // 2. Dropout
            if config.dropout_rate > 0.0 && rng.r#gen::<f32>() < config.dropout_rate {
                *v = 0.0;
            }
            // 3. Noise Injection
            else if config.noise_level > 0.0 {
                // Simple uniform noise scaled by noise_level
                let noise: f32 = rng.r#gen_range(-1.0..1.0) * config.noise_level;
                *v += noise;
            }
        }

        // Re-normalize if needed? For cosine, we might want to keep it normalized,
        // but a true glitch might break normalization! Let's re-normalize to keep it
        // comparable in standard semantic spaces, unless inversion/dropout zeroes it.
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for v in vec.iter_mut() {
                *v /= norm;
            }
        }

        self.db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector(property, &vec)
                    .build(),
            )
        })?;

        Ok(())
    }
}
"""
    content = content[:start] + impl_block + "\n" + content[end:]

with open("src/experimental/glitch.rs", "w") as f:
    f.write(content)
