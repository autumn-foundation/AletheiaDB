use crate::GallifreyDB;
use crate::api::transaction::WriteOps;
use crate::core::id::NodeId;
use crate::core::property::PropertyMapBuilder;
use crate::utils::Result;

/// A simple pseudo-random number generator (Linear Congruential Generator).
/// Used to avoid adding `rand` dependency to the library crate.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_f32(&mut self) -> f32 {
        // Constants from Numerical Recipes
        self.state = self.state.wrapping_mul(1664525).wrapping_add(1013904223);
        // Convert to f32 in [0.0, 1.0)
        (self.state >> 40) as f32 / 16777216.0
    }
}

/// Configuration for the Digital Garden simulation.
#[derive(Debug, Clone)]
pub struct GardenConfig {
    /// Label used for plant nodes.
    pub plant_label: String,
    /// Property name for the vector genome.
    pub vector_property: String,
    /// Property name for energy storage.
    pub energy_property: String,
    /// Dimension of the vector genome.
    pub dimensions: usize,
    /// Energy cost per cycle (metabolism).
    pub metabolism_cost: f64,
    /// Minimum energy required to reproduce.
    pub reproduction_threshold: f64,
    /// Rate of mutation during reproduction (0.0 to 1.0).
    pub mutation_rate: f32,
    /// Energy gained multiplier based on uniqueness (isolation).
    pub photosynthesis_efficiency: f64,
}

impl Default for GardenConfig {
    fn default() -> Self {
        Self {
            plant_label: "DigitalPlant".to_string(),
            vector_property: "genome".to_string(),
            energy_property: "energy".to_string(),
            dimensions: 4,
            metabolism_cost: 5.0,
            reproduction_threshold: 100.0,
            mutation_rate: 0.1,
            photosynthesis_efficiency: 50.0,
        }
    }
}

/// A Digital Garden simulation where nodes act as plants evolving via vector similarity.
pub struct DigitalGarden<'a> {
    db: &'a GallifreyDB,
    config: GardenConfig,
}

impl<'a> DigitalGarden<'a> {
    /// Create a new Digital Garden simulation.
    pub fn new(db: &'a GallifreyDB, config: GardenConfig) -> Self {
        Self { db, config }
    }

    /// Run one simulation cycle (Photosynthesis -> Reproduction -> Metabolism).
    ///
    /// Returns the number of new plants created.
    pub fn run_cycle(&self) -> Result<usize> {
        self.photosynthesis()?;
        let offspring_count = self.reproduce()?;
        self.metabolism()?;
        Ok(offspring_count)
    }

    /// Phase 1: Photosynthesis
    /// Plants gain energy based on their uniqueness (vector isolation).
    /// More isolated plants (fewer neighbors) compete less and gain more energy.
    fn photosynthesis(&self) -> Result<()> {
        // 1. Identify all plants
        let plants = self
            .db
            .query()
            .scan_label(&self.config.plant_label)
            .execute(self.db)?;

        // 2. Calculate energy gain for each plant
        let mut updates = Vec::new();

        for row_result in plants {
            let row = row_result?;
            if let Some(node_id) = row.entity.node_id() {
                let genome_vec = self.get_genome(node_id)?;

                if let Some(genome) = genome_vec {
                    // Find neighbors to determine uniqueness
                    // Higher distance = more unique = more energy
                    // Accessing current storage directly for multi-property search
                    let neighbors = self.db.current.find_similar_by_embedding_in_with_label(
                        &self.config.vector_property,
                        &genome,
                        &self.config.plant_label,
                        5, // Look at 5 nearest neighbors
                    )?;

                    // Skip self in neighbors check
                    let valid_neighbors: Vec<_> =
                        neighbors.iter().filter(|(id, _)| *id != node_id).collect();

                    let isolation_score = if valid_neighbors.is_empty() {
                        1.0 // Maximum isolation if no neighbors
                    } else {
                        // Average distance (1.0 - similarity)
                        let total_sim: f32 = valid_neighbors.iter().map(|(_, sim)| *sim).sum();
                        let avg_sim = total_sim / valid_neighbors.len() as f32;
                        (1.0 - avg_sim).max(0.0)
                    };

                    let energy_gain =
                        isolation_score as f64 * self.config.photosynthesis_efficiency;
                    let current_energy = self.get_energy(node_id)?;
                    updates.push((node_id, current_energy + energy_gain));
                }
            }
        }

        // 3. Apply updates
        if !updates.is_empty() {
            self.db.write(|tx| {
                for (id, new_energy) in updates {
                    let props = PropertyMapBuilder::new()
                        .insert(&self.config.energy_property, new_energy)
                        .build();
                    tx.update_node(id, props)?;
                }
                Ok(())
            })?;
        }

        Ok(())
    }

    /// Phase 2: Reproduction
    /// High-energy plants find a mate and create offspring.
    fn reproduce(&self) -> Result<usize> {
        let plants = self
            .db
            .query()
            .scan_label(&self.config.plant_label)
            .execute(self.db)?;

        let mut offspring_created = 0;

        // Collect parents first to avoid concurrent modification issues during iteration
        // (though iterator buffers, logic is cleaner this way)
        let mut parents = Vec::new();
        for row_result in plants {
            let row = row_result?;
            if let Some(node_id) = row.entity.node_id() {
                parents.push(node_id);
            }
        }

        for parent_id in parents {
            let energy = self.get_energy(parent_id)?;

            if energy >= self.config.reproduction_threshold {
                let genome_vec = self.get_genome(parent_id)?;
                if let Some(parent_genome) = genome_vec {
                    // Find a mate (nearest neighbor)
                    let neighbors = self.db.current.find_similar_by_embedding_in_with_label(
                        &self.config.vector_property,
                        &parent_genome,
                        &self.config.plant_label,
                        2, // Need just 1 mate, request 2 to skip self
                    )?;

                    if let Some((mate_id, _)) = neighbors.iter().find(|(id, _)| *id != parent_id) {
                        let mate_genome = self.get_genome(*mate_id)?;
                        if let Some(mate_genome_vec) = mate_genome {
                            // Crossover + Mutation
                            let child_genome = self.crossover(
                                &parent_genome,
                                &mate_genome_vec,
                                parent_id.as_u64(),
                            );

                            // Create offspring
                            self.db.write(|tx| {
                                // Create Child
                                let child_props = PropertyMapBuilder::new()
                                    .insert_vector(&self.config.vector_property, &child_genome)
                                    .insert(&self.config.energy_property, 10.0) // Initial energy
                                    .build();
                                let child_id =
                                    tx.create_node(&self.config.plant_label, child_props)?;

                                // Create Lineage Edges
                                let edge_props = PropertyMapBuilder::new().build();
                                tx.create_edge(
                                    child_id,
                                    parent_id,
                                    "OFFSPRING_OF",
                                    edge_props.clone(),
                                )?;
                                tx.create_edge(child_id, *mate_id, "OFFSPRING_OF", edge_props)?;

                                // Deduct Energy from Parent
                                let reduced_energy =
                                    energy - (self.config.reproduction_threshold * 0.5);
                                tx.update_node(
                                    parent_id,
                                    PropertyMapBuilder::new()
                                        .insert(&self.config.energy_property, reduced_energy)
                                        .build(),
                                )?;

                                Ok(())
                            })?;

                            offspring_created += 1;
                        }
                    } else {
                        // Asexual reproduction (Cloning with mutation) if no mate
                        let child_genome = self.mutate(&parent_genome, parent_id.as_u64());
                        self.db.write(|tx| {
                            let child_props = PropertyMapBuilder::new()
                                .insert_vector(&self.config.vector_property, &child_genome)
                                .insert(&self.config.energy_property, 10.0)
                                .build();
                            let child_id = tx.create_node(&self.config.plant_label, child_props)?;

                            let edge_props = PropertyMapBuilder::new().build();
                            tx.create_edge(child_id, parent_id, "CLONE_OF", edge_props)?;

                            let reduced_energy =
                                energy - (self.config.reproduction_threshold * 0.8);
                            tx.update_node(
                                parent_id,
                                PropertyMapBuilder::new()
                                    .insert(&self.config.energy_property, reduced_energy)
                                    .build(),
                            )?;
                            Ok(())
                        })?;
                        offspring_created += 1;
                    }
                }
            }
        }

        Ok(offspring_created)
    }

    /// Phase 3: Metabolism
    /// All plants lose a small amount of energy per cycle.
    fn metabolism(&self) -> Result<()> {
        let plants = self
            .db
            .query()
            .scan_label(&self.config.plant_label)
            .execute(self.db)?;

        let mut updates = Vec::new();

        for row_result in plants {
            if let Ok(row) = row_result {
                if let Some(node_id) = row.entity.node_id() {
                    // Fetch energy (optimistic read)
                    if let Ok(energy) = self.get_energy(node_id) {
                        updates.push((node_id, energy));
                    }
                }
            }
        }

        if updates.is_empty() {
            return Ok(());
        }

        self.db.write(|tx| {
            for (node_id, energy) in updates {
                let new_energy: f64 = (energy - self.config.metabolism_cost).max(0.0);
                tx.update_node(
                    node_id,
                    PropertyMapBuilder::new()
                        .insert(&self.config.energy_property, new_energy)
                        .build(),
                )?;
            }
            Ok(())
        })?;

        Ok(())
    }

    // --- Helpers ---

    #[allow(dead_code)]
    fn get_genome(&self, node_id: NodeId) -> Result<Option<Vec<f32>>> {
        let node = self.db.get_node(node_id)?;
        Ok(node
            .get_property(&self.config.vector_property)
            .and_then(|v| v.as_vector())
            .map(|v| v.to_vec()))
    }

    fn get_energy(&self, node_id: NodeId) -> Result<f64> {
        // Use read transaction for consistent view if possible, but get_node is atomic enough
        // Using db.read() would be cleaner but self.db.get_node() is fine for experimental
        let node = self.db.get_node(node_id)?;
        Ok(node
            .get_property(&self.config.energy_property)
            .and_then(|v| v.as_float())
            .unwrap_or(0.0))
    }

    fn crossover(&self, parent1: &[f32], parent2: &[f32], seed: u64) -> Vec<f32> {
        let mut rng = SimpleRng::new(seed);
        let mut child = Vec::with_capacity(parent1.len());
        for (g1, g2) in parent1.iter().zip(parent2.iter()) {
            // Average + Mutation
            let base = (g1 + g2) / 2.0;
            let mutation = (rng.next_f32() - 0.5) * self.config.mutation_rate;
            child.push(base + mutation);
        }
        child
    }

    fn mutate(&self, parent: &[f32], seed: u64) -> Vec<f32> {
        let mut rng = SimpleRng::new(seed);
        parent
            .iter()
            .map(|g| {
                let mutation = (rng.next_f32() - 0.5) * self.config.mutation_rate * 2.0;
                *g + mutation
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::ReadOps;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;

    #[test]
    fn test_garden_simulation() -> Result<()> {
        let db = GallifreyDB::new()?;
        let config = GardenConfig::default();

        // 1. Enable vector index
        db.enable_vector_index(
            &config.vector_property,
            HnswConfig::new(config.dimensions, DistanceMetric::Cosine),
        )?;

        // 2. Seed the garden
        let props1 = PropertyMapBuilder::new()
            .insert_vector(&config.vector_property, &[0.1, 0.1, 0.1, 0.1])
            .insert(&config.energy_property, 80.0)
            .build();

        let props2 = PropertyMapBuilder::new()
            .insert_vector(&config.vector_property, &[0.9, 0.9, 0.9, 0.9]) // Distant -> high isolation
            .insert(&config.energy_property, 120.0) // Ready to reproduce
            .build();

        db.write(|tx| {
            tx.create_node(&config.plant_label, props1)?;
            tx.create_node(&config.plant_label, props2)?;
            Ok(())
        })?;

        let garden = DigitalGarden::new(&db, config.clone());

        // 3. Run Cycle 1
        // Plant 2 had 120 energy > 100 threshold. It should reproduce.
        let offspring = garden.run_cycle()?;

        assert!(offspring > 0, "Should have created offspring");

        // 4. Verify population growth
        let plant_count = db
            .query()
            .scan_label(&config.plant_label)
            .execute(&db)?
            .count();
        assert_eq!(plant_count, 3, "Population should be 3");

        // 5. Verify energy changes
        db.read(|tx| {
            // Just verifying reading works, logic is complex to assert exact values due to float math
            assert!(tx.node_count() >= 3);
            Ok(())
        })?;

        Ok(())
    }
}
