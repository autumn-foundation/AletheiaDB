use crate::api::transaction::WriteOps;
use crate::core::id::NodeId;
use crate::core::property::PropertyMapBuilder;
use crate::db::GallifreyDB;
use crate::index::vector::{DistanceMetric, HnswConfig};
use crate::utils::Result;
use rand::Rng;
use std::sync::Arc;

// 3 dimensions: X, Y, GeneticTrait
const DIMENSIONS: usize = 3;

/// A simulated plant entity in the digital garden.
#[derive(Debug, Clone)]
pub struct Plant {
    /// The unique ID of the node in the graph.
    pub id: NodeId,
    /// X coordinate (0.0 - 1.0).
    pub x: f32,
    /// Y coordinate (0.0 - 1.0).
    pub y: f32,
    /// Genetic trait value determining color/characteristics.
    pub trait_val: f32,
    /// Current health status (0.0 - 1.0).
    pub health: f32,
    /// Generation number.
    pub generation: u64,
}

impl Plant {
    /// Returns the vector representation of the plant's genome (X, Y, Trait).
    pub fn vector(&self) -> Vec<f32> {
        vec![self.x, self.y, self.trait_val]
    }
}

/// Simulation engine for "The Memetic Garden".
///
/// This struct manages the state of the garden and interacts with GallifreyDB
/// to perform spatial queries and store plant data.
pub struct DigitalGarden {
    db: Arc<GallifreyDB>,
    plants: Vec<Plant>,
}

impl DigitalGarden {
    /// Create a new garden simulation.
    ///
    /// This will initialize the vector index for "genome" if it doesn't exist.
    pub fn new(db: Arc<GallifreyDB>) -> Result<Self> {
        // Enable vector index on "genome" property if not present
        if !db.has_vector_index("genome") {
            let config = HnswConfig::new(DIMENSIONS, DistanceMetric::Euclidean);
            db.enable_vector_index("genome", config)?;
        }

        Ok(Self {
            db,
            plants: Vec::new(),
        })
    }

    /// Seed the garden with N random plants.
    pub fn seed(&mut self, count: usize) -> Result<()> {
        let mut rng = rand::thread_rng();
        for _ in 0..count {
            let x = rng.r#gen::<f32>();
            let y = rng.r#gen::<f32>();
            let trait_val = rng.r#gen::<f32>();

            let genome = vec![x, y, trait_val];

            let props = PropertyMapBuilder::new()
                .insert("type", "Plant")
                .insert("health", 1.0)
                .insert("generation", 0i64)
                .insert_vector("genome", &genome)
                .build();

            let id = self.db.create_node("Plant", props)?;

            self.plants.push(Plant {
                id,
                x,
                y,
                trait_val,
                health: 1.0,
                generation: 0,
            });
        }
        Ok(())
    }

    /// Advance simulation by one tick.
    ///
    /// This performs the following steps:
    /// 1. **Growth & Competition**: Queries neighbors using HNSW. Overcrowding reduces health.
    /// 2. **Reproduction**: Healthy plants reproduce with mutation.
    /// 3. **Death**: Dead plants are removed from the simulation and database.
    pub fn tick(&mut self) -> Result<()> {
        let mut new_plants = Vec::new();
        let mut dead_indices = Vec::new();

        // 1. Growth & Competition
        for (i, plant) in self.plants.iter_mut().enumerate() {
            // Search specifically in the 'genome' index
            // We search for k=6 neighbors (approx)
            let neighbors = self.db.find_similar_in("genome", plant.id, 6)?;

            // Competition: too many neighbors = health loss
            // Neighbors includes self (distance ~0), so we ignore distance < epsilon
            // Or better, filter by ID if find_similar_in returns NodeId
            let overcrowded = neighbors
                .iter()
                .filter(|(id, dist)| *id != plant.id && *dist < 0.1) // 0.1 radius
                .count();

            if overcrowded > 3 {
                plant.health -= 0.1;
            } else {
                plant.health += 0.05;
            }

            plant.health = plant.health.clamp(0.0, 1.0);

            if plant.health <= 0.0 {
                dead_indices.push(i);
                continue;
            }

            // Reproduction: if healthy and not overcrowded
            if plant.health > 0.8 && overcrowded < 3 {
                let mut rng = rand::thread_rng();
                if rng.gen_bool(0.05) {
                    // 5% chance
                    let mutation_rate = 0.05;
                    let new_x = (plant.x + rng.gen_range(-mutation_rate..mutation_rate))
                        .clamp(0.0, 1.0);
                    let new_y = (plant.y + rng.gen_range(-mutation_rate..mutation_rate))
                        .clamp(0.0, 1.0);
                    let new_trait = (plant.trait_val
                        + rng.gen_range(-mutation_rate..mutation_rate))
                    .clamp(0.0, 1.0);

                    let genome = vec![new_x, new_y, new_trait];
                    let props = PropertyMapBuilder::new()
                        .insert("type", "Plant")
                        .insert("health", 0.5)
                        .insert("generation", (plant.generation + 1) as i64)
                        .insert_vector("genome", &genome)
                        .build();

                    let id = self.db.create_node("Plant", props)?;
                    new_plants.push(Plant {
                        id,
                        x: new_x,
                        y: new_y,
                        trait_val: new_trait,
                        health: 0.5,
                        generation: plant.generation + 1,
                    });
                }
            }
        }

        // Remove dead plants from local state and DB
        dead_indices.sort_unstable_by(|a, b| b.cmp(a));
        for i in dead_indices {
            let id = self.plants[i].id;
            // Delete from DB
            let _ = self.db.write(|tx| tx.delete_node(id));
            self.plants.remove(i);
        }

        self.plants.append(&mut new_plants);

        Ok(())
    }

    /// Get the current list of plants in the garden.
    pub fn get_plants(&self) -> &Vec<Plant> {
        &self.plants
    }

    /// Get the current population count.
    pub fn population(&self) -> usize {
        self.plants.len()
    }
}
