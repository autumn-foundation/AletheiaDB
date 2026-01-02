//! Doctor Who Demo - A fun exploration of GallifreyDB's bi-temporal capabilities
//!
//! This demo creates a knowledge graph about the Doctor Who universe,
//! perfect for demonstrating temporal versioning since:
//! - The Doctor regenerates (entity versioning)
//! - Companions come and go (relationship changes over time)
//! - Knowledge about characters evolves (property updates)
//!
//! Run with: cargo run --example doctor_who_demo

use gallifreydb::{
    GLOBAL_INTERNER, GallifreyDB, InternedString, NodeId, PropertyMapBuilder, Result, Timestamp,
    WriteOps,
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// Helper to get label string from InternedString
fn label_str(label: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve(label)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{:?}", label))
}

/// Helper to format property values nicely
fn format_value(value: &gallifreydb::PropertyValue) -> String {
    use gallifreydb::PropertyValue;
    match value {
        PropertyValue::Null => "null".to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Bytes(b) => format!("<{} bytes>", b.len()),
        PropertyValue::Array(arr) => format!("[{} items]", arr.len()),
        PropertyValue::Vector(v) => format!("<vector dim={}>", v.len()),
    }
}

/// Get current timestamp in microseconds
fn now_timestamp() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as Timestamp
}

/// Format timestamp for display
#[allow(dead_code)]
fn format_timestamp(ts: Timestamp) -> String {
    // Convert microseconds to a readable format
    let secs = ts / 1_000_000;
    chrono_lite_format(secs)
}

/// Simple timestamp formatting (no external deps)
fn chrono_lite_format(unix_secs: i64) -> String {
    // Just show relative time for simplicity
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let diff = now_secs - unix_secs;

    if diff < 1 {
        "just now".to_string()
    } else if diff < 60 {
        format!("{} seconds ago", diff)
    } else if diff < 3600 {
        format!("{} minutes ago", diff / 60)
    } else {
        format!("{} hours ago", diff / 3600)
    }
}

/// A regeneration event for tracking Doctor history
#[derive(Clone)]
struct RegenerationEvent {
    #[allow(dead_code)]
    timestamp: Timestamp,
    actor: String,
    incarnation: String,
    regen_number: i64,
    era: String,
}

/// Helper to create properties more easily
macro_rules! props {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut builder = PropertyMapBuilder::new();
        $(builder = builder.insert($key, $value);)*
        builder.build()
    }};
}

/// Stores node IDs and temporal events for easy reference
struct DemoData {
    nodes: HashMap<String, NodeId>,
    db: GallifreyDB,
    /// Track all Doctor regenerations with timestamps for time-travel demos
    regenerations: Vec<RegenerationEvent>,
    /// When the database was first populated
    #[allow(dead_code)]
    creation_time: Timestamp,
}

impl DemoData {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            db: GallifreyDB::new(),
            regenerations: Vec::new(),
            creation_time: now_timestamp(),
        }
    }

    fn get(&self, name: &str) -> Option<NodeId> {
        self.nodes.get(name).copied()
    }

    /// Record a regeneration event
    fn record_regeneration(
        &mut self,
        actor: String,
        incarnation: String,
        regen_number: i64,
        era: String,
    ) {
        self.regenerations.push(RegenerationEvent {
            timestamp: now_timestamp(),
            actor,
            incarnation,
            regen_number,
            era,
        });
    }
}

/// All the canonical Doctor incarnations (actor, incarnation, regen#, era)
const DOCTOR_INCARNATIONS: &[(&str, &str, i64, &str)] = &[
    ("William Hartnell", "First Doctor", 1, "1963-1966"),
    ("Patrick Troughton", "Second Doctor", 2, "1966-1969"),
    ("Jon Pertwee", "Third Doctor", 3, "1970-1974"),
    ("Tom Baker", "Fourth Doctor", 4, "1974-1981"),
    ("Peter Davison", "Fifth Doctor", 5, "1981-1984"),
    ("Colin Baker", "Sixth Doctor", 6, "1984-1986"),
    ("Sylvester McCoy", "Seventh Doctor", 7, "1987-1989"),
    ("Paul McGann", "Eighth Doctor", 8, "1996, 2013"),
    ("John Hurt", "War Doctor", 9, "2013 (50th)"), // The forgotten incarnation
    ("Christopher Eccleston", "Ninth Doctor", 10, "2005"),
    ("David Tennant", "Tenth Doctor", 11, "2005-2010"),
    ("Matt Smith", "Eleventh Doctor", 12, "2010-2013"),
    ("Peter Capaldi", "Twelfth Doctor", 13, "2014-2017"),
    ("Jodie Whittaker", "Thirteenth Doctor", 14, "2018-2022"),
    ("David Tennant", "Fourteenth Doctor", 15, "2023"), // Bi-generation!
    ("Ncuti Gatwa", "Fifteenth Doctor", 16, "2023-"),
];

fn populate_database(demo: &mut DemoData) -> Result<()> {
    println!("\n>>> Populating the Whoniverse...\n");

    // === TIME LORDS ===
    println!("  Creating Time Lords...");

    // Start with the First Doctor (William Hartnell, 1963)
    let (first_actor, first_incarnation, _, first_era) = DOCTOR_INCARNATIONS[0];
    let doctor = demo.db.create_node(
        "TimeLord",
        props! {
            "name" => "The Doctor",
            "species" => "Time Lord",
            "home_planet" => "Gallifrey",
            "regeneration_count" => 1i64,
            "current_incarnation" => first_incarnation,
            "actor" => first_actor,
            "first_appearance" => 1963i64,
            "catchphrase" => "Hmm? What's that, my boy?",
        },
    )?;
    demo.nodes.insert("doctor".to_string(), doctor);

    // Record first incarnation
    demo.regenerations.push(RegenerationEvent {
        timestamp: now_timestamp(),
        actor: first_actor.to_string(),
        incarnation: first_incarnation.to_string(),
        regen_number: 1,
        era: first_era.to_string(),
    });

    let master = demo.db.create_node(
        "TimeLord",
        props! {
            "name" => "The Master",
            "species" => "Time Lord",
            "home_planet" => "Gallifrey",
            "current_incarnation" => "Sacha Dhawan Master",
            "alignment" => "villain",
            "relationship_to_doctor" => "childhood friend turned enemy",
        },
    )?;
    demo.nodes.insert("master".to_string(), master);

    let romana = demo.db.create_node(
        "TimeLord",
        props! {
            "name" => "Romanadvoratrelundar",
            "nickname" => "Romana",
            "species" => "Time Lord",
            "home_planet" => "Gallifrey",
            "incarnations" => 2i64,
        },
    )?;
    demo.nodes.insert("romana".to_string(), romana);

    // === COMPANIONS ===
    println!("  Creating Companions...");

    let rose = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Rose Tyler",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Ninth/Tenth Doctor",
            "first_appearance" => 2005i64,
            "status" => "living in parallel universe",
        },
    )?;
    demo.nodes.insert("rose".to_string(), rose);

    let donna = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Donna Noble",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Tenth Doctor",
            "special_status" => "DoctorDonna",
            "catchphrase" => "Oi, spaceman!",
        },
    )?;
    demo.nodes.insert("donna".to_string(), donna);

    let amy = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Amelia Pond",
            "nickname" => "Amy",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Eleventh Doctor",
            "relationship" => "The Girl Who Waited",
        },
    )?;
    demo.nodes.insert("amy".to_string(), amy);

    let rory = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Rory Williams",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Eleventh Doctor",
            "special_status" => "The Last Centurion",
            "married_to" => "Amy Pond",
        },
    )?;
    demo.nodes.insert("rory".to_string(), rory);

    let clara = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Clara Oswald",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Eleventh/Twelfth Doctor",
            "nickname" => "The Impossible Girl",
        },
    )?;
    demo.nodes.insert("clara".to_string(), clara);

    let bill = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Bill Potts",
            "species" => "Human (later Heather-entity)",
            "home_planet" => "Earth",
            "era" => "Twelfth Doctor",
        },
    )?;
    demo.nodes.insert("bill".to_string(), bill);

    let ruby = demo.db.create_node(
        "Companion",
        props! {
            "name" => "Ruby Sunday",
            "species" => "Human",
            "home_planet" => "Earth",
            "era" => "Fifteenth Doctor",
            "mystery" => "unknown parentage",
        },
    )?;
    demo.nodes.insert("ruby".to_string(), ruby);

    // === ENEMIES ===
    println!("  Creating Enemies...");

    let daleks = demo.db.create_node(
        "Species",
        props! {
            "name" => "Daleks",
            "species" => "Dalek",
            "home_planet" => "Skaro",
            "creator" => "Davros",
            "weakness" => "stairs (historically)",
            "catchphrase" => "EXTERMINATE!",
            "threat_level" => 10i64,
        },
    )?;
    demo.nodes.insert("daleks".to_string(), daleks);

    let cybermen = demo.db.create_node(
        "Species",
        props! {
            "name" => "Cybermen",
            "species" => "Cyberman",
            "origin" => "Mondas/Pete's World",
            "goal" => "upgrade all life",
            "catchphrase" => "DELETE!",
            "threat_level" => 9i64,
        },
    )?;
    demo.nodes.insert("cybermen".to_string(), cybermen);

    let weeping_angels = demo.db.create_node(
        "Species",
        props! {
            "name" => "Weeping Angels",
            "species" => "Weeping Angel",
            "ability" => "quantum-locked",
            "attack_method" => "sends victims back in time",
            "weakness" => "being observed",
            "first_appearance" => 2007i64,
            "threat_level" => 8i64,
        },
    )?;
    demo.nodes
        .insert("weeping_angels".to_string(), weeping_angels);

    let silence = demo.db.create_node(
        "Species",
        props! {
            "name" => "The Silence",
            "species" => "Silence",
            "ability" => "forgotten when not observed",
            "goal" => "prevent the Doctor from reaching Trenzalore",
        },
    )?;
    demo.nodes.insert("silence".to_string(), silence);

    let davros = demo.db.create_node(
        "Character",
        props! {
            "name" => "Davros",
            "species" => "Kaled",
            "home_planet" => "Skaro",
            "role" => "Creator of the Daleks",
            "alignment" => "villain",
        },
    )?;
    demo.nodes.insert("davros".to_string(), davros);

    // === PLACES ===
    println!("  Creating Locations...");

    let gallifrey = demo.db.create_node(
        "Planet",
        props! {
            "name" => "Gallifrey",
            "location" => "Kasterborous constellation",
            "inhabitants" => "Time Lords",
            "status" => "exists in pocket universe",
            "notable_features" => "twin suns, Citadel of the Time Lords",
        },
    )?;
    demo.nodes.insert("gallifrey".to_string(), gallifrey);

    let earth = demo.db.create_node(
        "Planet",
        props! {
            "name" => "Earth",
            "location" => "Sol system",
            "designation" => "Sol 3",
            "protected_by" => "The Doctor",
            "notable_feature" => "Level 5 planet",
        },
    )?;
    demo.nodes.insert("earth".to_string(), earth);

    let skaro = demo.db.create_node(
        "Planet",
        props! {
            "name" => "Skaro",
            "location" => "unknown",
            "inhabitants" => "Daleks (formerly Kaleds and Thals)",
            "status" => "repeatedly destroyed and rebuilt",
        },
    )?;
    demo.nodes.insert("skaro".to_string(), skaro);

    let tardis = demo.db.create_node(
        "Vehicle",
        props! {
            "name" => "TARDIS",
            "full_name" => "Time And Relative Dimension In Space",
            "type" => "Type 40 TT Capsule",
            "owner" => "The Doctor",
            "appearance" => "1960s Police Box",
            "interior" => "bigger on the inside",
            "sentient" => true,
        },
    )?;
    demo.nodes.insert("tardis".to_string(), tardis);

    // === RELATIONSHIPS ===
    println!("  Creating Relationships...");

    // Doctor's relationships
    demo.db.create_edge(
        doctor,
        tardis,
        "OWNS",
        props! { "since" => "stole it", "bond" => "telepathic" },
    )?;

    demo.db.create_edge(
        doctor,
        gallifrey,
        "FROM",
        props! { "relationship" => "complicated" },
    )?;

    demo.db.create_edge(
        doctor,
        earth,
        "PROTECTS",
        props! { "reason" => "favorite planet" },
    )?;

    // Companions travel with the Doctor
    for companion in ["rose", "donna", "amy", "rory", "clara", "bill", "ruby"] {
        if let Some(comp_id) = demo.get(companion) {
            demo.db.create_edge(
                doctor,
                comp_id,
                "TRAVELS_WITH",
                props! { "relationship" => "companion" },
            )?;
        }
    }

    // Amy and Rory are married
    demo.db.create_edge(
        amy,
        rory,
        "MARRIED_TO",
        props! { "wedding" => "Big Bang 2" },
    )?;

    // Enemies and Doctor
    demo.db.create_edge(
        doctor,
        daleks,
        "ENEMY_OF",
        props! { "since" => "Time War", "battles" => "countless" },
    )?;

    demo.db.create_edge(
        doctor,
        cybermen,
        "ENEMY_OF",
        props! { "first_encounter" => 1966i64 },
    )?;

    demo.db.create_edge(
        doctor,
        weeping_angels,
        "ENEMY_OF",
        props! { "strategy" => "don't blink" },
    )?;

    demo.db.create_edge(
        doctor,
        master,
        "ENEMY_OF",
        props! { "originally" => "friends", "complexity" => "frenemy" },
    )?;

    // Davros created Daleks
    demo.db.create_edge(
        davros,
        daleks,
        "CREATED",
        props! { "purpose" => "survival" },
    )?;

    // Daleks from Skaro
    demo.db.create_edge(daleks, skaro, "FROM", props! {})?;

    // Master is also a Time Lord
    demo.db.create_edge(master, gallifrey, "FROM", props! {})?;

    // Romana traveled with Doctor
    demo.db.create_edge(
        doctor,
        romana,
        "TRAVELS_WITH",
        props! { "era" => "Fourth Doctor" },
    )?;

    println!(
        "\n>>> Database populated with {} nodes and {} edges!",
        demo.db.node_count(),
        demo.db.edge_count()
    );

    // Now perform all the Doctor's regenerations to create temporal history!
    println!("\n  Regenerating through all of the Doctor's incarnations...");

    for (actor, incarnation, regen_num, era) in DOCTOR_INCARNATIONS.iter().skip(1) {
        // Create a new version of the Doctor for each incarnation
        let actor_owned = actor.to_string();
        let incarnation_owned = incarnation.to_string();
        let regen = *regen_num;

        demo.db.write(|tx| {
            tx.update_node(
                doctor,
                props! {
                    "actor" => actor_owned.as_str(),
                    "current_incarnation" => incarnation_owned.as_str(),
                    "regeneration_count" => regen,
                },
            )
        })?;

        // Record the regeneration event
        demo.regenerations.push(RegenerationEvent {
            timestamp: now_timestamp(),
            actor: actor.to_string(),
            incarnation: incarnation.to_string(),
            regen_number: *regen_num,
            era: era.to_string(),
        });

        // Small delay to ensure distinct timestamps
        std::thread::sleep(std::time::Duration::from_micros(100));
    }

    println!(
        "  Created {} temporal versions of The Doctor!",
        DOCTOR_INCARNATIONS.len()
    );

    Ok(())
}

fn print_node_details(demo: &DemoData, name: &str) -> Result<()> {
    if let Some(id) = demo.get(name) {
        let node = demo.db.get_node(id)?;
        println!("\n=== {} ===", name.to_uppercase());
        println!("Label: {}", label_str(node.label));
        println!("Properties:");

        // Sort properties for consistent display
        let mut props: Vec<_> = node.properties.iter().collect();
        props.sort_by_key(|(k, _)| k.as_str());

        for (key, value) in props {
            println!("  {}: {}", key, format_value(value));
        }

        // Show relationships
        let outgoing = demo.db.get_outgoing_edges(id);
        if !outgoing.is_empty() {
            println!("Outgoing relationships:");
            for edge_id in outgoing {
                if let Ok(edge) = demo.db.get_edge(edge_id) {
                    let target = demo.db.get_node(edge.target)?;
                    let target_name = target
                        .properties
                        .get("name")
                        .map(format_value)
                        .unwrap_or_else(|| label_str(target.label));
                    println!("  --[{}]--> {}", label_str(edge.label), target_name);
                }
            }
        }

        let incoming = demo.db.get_incoming_edges(id);
        if !incoming.is_empty() {
            println!("Incoming relationships:");
            for edge_id in incoming {
                if let Ok(edge) = demo.db.get_edge(edge_id) {
                    let source = demo.db.get_node(edge.source)?;
                    let source_name = source
                        .properties
                        .get("name")
                        .map(format_value)
                        .unwrap_or_else(|| label_str(source.label));
                    println!("  <--[{}]-- {}", label_str(edge.label), source_name);
                }
            }
        }
    } else {
        println!("Unknown entity: {}", name);
        println!("Available: doctor, master, romana, rose, donna, amy, rory, clara, bill, ruby,");
        println!("           daleks, cybermen, weeping_angels, silence, davros,");
        println!("           gallifrey, earth, skaro, tardis");
    }
    Ok(())
}

fn list_all_nodes(demo: &DemoData) {
    println!("\n=== ALL ENTITIES IN THE WHONIVERSE ===\n");

    let mut by_label: HashMap<String, Vec<(String, NodeId)>> = HashMap::new();

    for (name, &id) in &demo.nodes {
        if let Ok(node) = demo.db.get_node(id) {
            let label = label_str(node.label);
            by_label.entry(label).or_default().push((name.clone(), id));
        }
    }

    let mut labels: Vec<_> = by_label.keys().cloned().collect();
    labels.sort();

    for label in labels {
        println!("{}:", label);
        let mut entities = by_label[&label].clone();
        entities.sort_by_key(|(name, _)| name.clone());
        for (name, id) in entities {
            if let Ok(node) = demo.db.get_node(id) {
                let display_name = node
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_else(|| name.clone());
                println!("  {} (key: {})", display_name, name);
            }
        }
        println!();
    }
}

fn show_companions(demo: &DemoData) -> Result<()> {
    println!("\n=== THE DOCTOR'S COMPANIONS ===\n");

    if let Some(doctor_id) = demo.get("doctor") {
        let edges = demo.db.get_outgoing_edges(doctor_id);
        for edge_id in edges {
            if let Ok(edge) = demo.db.get_edge(edge_id) {
                if label_str(edge.label) != "TRAVELS_WITH" {
                    continue;
                }
                let companion = demo.db.get_node(edge.target)?;
                let name = companion
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_else(|| "Unknown".to_string());
                let era = companion
                    .properties
                    .get("era")
                    .map(format_value)
                    .unwrap_or_else(|| "Unknown era".to_string());
                println!("  {} - {}", name, era);
            }
        }
    }
    Ok(())
}

fn show_enemies(demo: &DemoData) -> Result<()> {
    println!("\n=== THE DOCTOR'S ENEMIES ===\n");

    if let Some(doctor_id) = demo.get("doctor") {
        let edges = demo.db.get_outgoing_edges(doctor_id);
        for edge_id in edges {
            if let Ok(edge) = demo.db.get_edge(edge_id) {
                if label_str(edge.label) != "ENEMY_OF" {
                    continue;
                }
                let enemy = demo.db.get_node(edge.target)?;
                let name = enemy
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_else(|| "Unknown".to_string());
                let threat = enemy
                    .properties
                    .get("threat_level")
                    .map(|v| format!("(Threat Level: {})", format_value(v)))
                    .unwrap_or_default();
                println!("  {} {}", name, threat);
            }
        }
    }
    Ok(())
}

/// Convert a number to its ordinal name (e.g., 14 -> "Fourteenth")
fn ordinal_name(n: i64) -> String {
    match n {
        1 => "First".to_string(),
        2 => "Second".to_string(),
        3 => "Third".to_string(),
        4 => "Fourth".to_string(),
        5 => "Fifth".to_string(),
        6 => "Sixth".to_string(),
        7 => "Seventh".to_string(),
        8 => "Eighth".to_string(),
        9 => "Ninth".to_string(),
        10 => "Tenth".to_string(),
        11 => "Eleventh".to_string(),
        12 => "Twelfth".to_string(),
        13 => "Thirteenth".to_string(),
        14 => "Fourteenth".to_string(),
        15 => "Fifteenth".to_string(),
        16 => "Sixteenth".to_string(),
        17 => "Seventeenth".to_string(),
        18 => "Eighteenth".to_string(),
        19 => "Nineteenth".to_string(),
        20 => "Twentieth".to_string(),
        _ => format!("{}th", n),
    }
}

fn update_doctor_incarnation(
    demo: &mut DemoData,
    new_actor: &str,
    incarnation: &str,
) -> Result<()> {
    if let Some(doctor_id) = demo.get("doctor") {
        // Get current regeneration count
        let current = demo.db.get_node(doctor_id)?;
        let regen_count = current
            .properties
            .get("regeneration_count")
            .and_then(|v| {
                if let gallifreydb::PropertyValue::Int(n) = v {
                    Some(*n)
                } else {
                    None
                }
            })
            .unwrap_or(13);

        // Get previous incarnation info for the log
        let prev_actor = current
            .properties
            .get("actor")
            .map(format_value)
            .unwrap_or_else(|| "Unknown".to_string());
        let prev_incarnation = current
            .properties
            .get("current_incarnation")
            .map(format_value)
            .unwrap_or_else(|| "Unknown".to_string());

        // Generate incarnation name if using default
        let incarnation_final = if incarnation == "Next Doctor" {
            format!("{} Doctor", ordinal_name(regen_count + 1))
        } else {
            incarnation.to_string()
        };

        // Capture values for the closure
        let new_actor_owned = new_actor.to_string();
        let incarnation_owned = incarnation_final.clone();

        demo.db.write(|tx| {
            tx.update_node(
                doctor_id,
                props! {
                    "actor" => new_actor_owned.as_str(),
                    "current_incarnation" => incarnation_owned.as_str(),
                    "regeneration_count" => regen_count + 1,
                },
            )
        })?;

        // Record the regeneration event
        demo.record_regeneration(
            new_actor.to_string(),
            incarnation_final.clone(),
            regen_count + 1,
            "Future".to_string(), // User-added Doctors are from the future!
        );

        println!("\n>>> REGENERATION EVENT!");
        println!(
            ">>> {} ({}) --> {} ({})",
            prev_actor, prev_incarnation, new_actor, incarnation_final
        );
        println!(">>> Regeneration #{}", regen_count + 1);
        println!("\n>>> A new temporal version was created!");
        println!(
            ">>> Use 'timeline' to see all incarnations, or 'timewarp <n>' to query past states."
        );
    }
    Ok(())
}

fn show_stats(demo: &DemoData) -> Result<()> {
    println!("\n=== DATABASE STATISTICS ===\n");
    println!("Total nodes: {}", demo.db.node_count());
    println!("Total edges: {}", demo.db.edge_count());

    let stats = demo.db.historical_stats()?;
    println!("\nHistorical storage:");
    println!("  Total node versions: {}", stats.total_node_versions);
    println!("  Total edge versions: {}", stats.total_edge_versions);
    println!("  Node anchors: {}", stats.node_anchor_count);
    println!("  Node deltas: {}", stats.node_delta_count);
    println!("  Edge anchors: {}", stats.edge_anchor_count);
    println!("  Edge deltas: {}", stats.edge_delta_count);

    if stats.node_delta_count > 0 {
        println!(
            "\n  Compression ratio: {:.1}%",
            stats.compression_ratio() * 100.0
        );
    }
    Ok(())
}

fn show_timeline(demo: &DemoData) {
    println!("\n=== THE DOCTOR'S TIMELINE ===\n");

    if demo.regenerations.is_empty() {
        println!("  No regeneration events recorded yet.");
        return;
    }

    println!("  #   | Actor                | Incarnation           | Era");
    println!("  ----|----------------------|-----------------------|------------");

    for (i, event) in demo.regenerations.iter().enumerate() {
        let marker = if i == demo.regenerations.len() - 1 {
            ">>>" // Current incarnation
        } else {
            "   "
        };
        println!(
            "{} {:2}  | {:20} | {:21} | {}",
            marker, event.regen_number, event.actor, event.incarnation, event.era
        );
    }

    println!("\n  >>> = Current incarnation");
    println!("  Use 'timewarp <#>' to query a past incarnation (e.g., 'timewarp 4' for Tom Baker)");
}

fn timewarp_query(demo: &DemoData, index: usize) -> Result<()> {
    if demo.regenerations.is_empty() {
        println!("No regeneration history available. Try 'regenerate' first!");
        return Ok(());
    }

    if index == 0 || index > demo.regenerations.len() {
        println!(
            "Invalid incarnation number. Valid range: 1-{}",
            demo.regenerations.len()
        );
        println!("Use 'timeline' to see all available incarnations.");
        return Ok(());
    }

    let target_event = &demo.regenerations[index - 1];
    let is_current = index == demo.regenerations.len();

    println!(
        "\n=== TIME WARP: Incarnation #{} ===",
        target_event.regen_number
    );
    println!("=================================\n");

    if is_current {
        println!("  Status: CURRENT (present day)");
    } else {
        println!("  Status: HISTORICAL (past incarnation)");
    }

    // Show the historical state from our event log
    println!("\n  Doctor's State at this point:");
    println!("  ┌────────────────────────────────────────┐");
    println!("  │  Actor:        {:24} │", target_event.actor);
    println!("  │  Incarnation:  {:24} │", target_event.incarnation);
    println!("  │  Era:          {:24} │", target_event.era);
    println!(
        "  │  Regeneration: {:24} │",
        format!("#{}", target_event.regen_number)
    );
    println!("  └────────────────────────────────────────┘");

    // Show temporal context
    if !is_current {
        println!("\n  Temporal Context:");

        // What came before
        if index > 1 {
            let prev = &demo.regenerations[index - 2];
            println!("    Previous: {} ({})", prev.actor, prev.incarnation);
        } else {
            println!("    Previous: (Beginning of recorded history)");
        }

        // What came after
        let next = &demo.regenerations[index];
        println!("    Next:     {} ({})", next.actor, next.incarnation);

        println!("\n  This state was SUPERSEDED when the Doctor regenerated.");
        println!("  In a real application, you could query this historical state using:");
        println!("    db.get_node_at_time(doctor_id, valid_time, transaction_time)");
    }

    // Show proof that the database has this versioned
    if demo.get("doctor").is_some() {
        let stats = demo.db.historical_stats()?;
        println!("\n  Database Proof:");
        println!(
            "    Total Doctor versions stored: {}",
            demo.regenerations.len()
        );
        println!(
            "    Anchors: {}, Deltas: {}",
            stats.node_anchor_count, stats.node_delta_count
        );
        println!("    (The database preserves ALL historical states!)");
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"
=== GALLIFREYDB DOCTOR WHO DEMO ===

Commands:
  list                    - List all entities in the database
  show <name>             - Show details for an entity (e.g., 'show doctor')
  companions              - List all of the Doctor's companions
  enemies                 - List all of the Doctor's enemies

TEMPORAL COMMANDS (the fun stuff!):
  regenerate <actor>      - Regenerate the Doctor (creates new temporal version!)
  timeline                - View the Doctor's complete regeneration timeline
  timewarp <#>            - Query a past incarnation using time-travel!
  stats                   - Show database statistics including version counts

  help                    - Show this help message
  quit / exit             - Exit the demo

Entity names you can use with 'show':
  Time Lords: doctor, master, romana
  Companions: rose, donna, amy, rory, clara, bill, ruby
  Enemies: daleks, cybermen, weeping_angels, silence, davros
  Places: gallifrey, earth, skaro, tardis

Try this temporal workflow:
  > timeline                                       # See ALL 16 incarnations!
  > timewarp 4                                     # Travel back to Tom Baker!
  > timewarp 11                                    # Visit David Tennant's era!
  > show doctor                                    # See current state (Ncuti Gatwa)
  > regenerate "Richard E. Grant"                  # Add the 17th Doctor!
  > timeline                                       # See your new incarnation!
"#
    );
}

fn main() -> Result<()> {
    println!(
        r#"
    ╔══════════════════════════════════════════════════════════════╗
    ║                                                              ║
    ║   ██████╗  █████╗ ██╗     ██╗     ██╗███████╗██████╗ ███████╗ ║
    ║  ██╔════╝ ██╔══██╗██║     ██║     ██║██╔════╝██╔══██╗╚══██╔══╝║
    ║  ██║  ███╗███████║██║     ██║     ██║█████╗  ██████╔╝   ██║   ║
    ║  ██║   ██║██╔══██║██║     ██║     ██║██╔══╝  ██╔══██╗   ██║   ║
    ║  ╚██████╔╝██║  ██║███████╗███████╗██║██║     ██║  ██║   ██║   ║
    ║   ╚═════╝ ╚═╝  ╚═╝╚══════╝╚══════╝╚═╝╚═╝     ╚═╝  ╚═╝   ╚═╝   ║
    ║                                                              ║
    ║              Doctor Who Universe Demo                        ║
    ║         Bi-Temporal Graph Database Explorer                  ║
    ╚══════════════════════════════════════════════════════════════╝
"#
    );

    let mut demo = DemoData::new();
    populate_database(&mut demo)?;

    print_help();

    loop {
        print!("\nwhoniverse> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let command = parts[0].to_lowercase();
        let args = parts.get(1).map(|s| s.trim()).unwrap_or("");

        match command.as_str() {
            "quit" | "exit" | "q" => {
                println!("\nAllons-y! Until next time...\n");
                break;
            }
            "help" | "h" | "?" => print_help(),
            "list" | "ls" => list_all_nodes(&demo),
            "show" | "get" | "s" => {
                if args.is_empty() {
                    println!("Usage: show <entity_name>");
                    println!("Example: show doctor");
                } else {
                    print_node_details(&demo, args)?;
                }
            }
            "companions" | "comp" => show_companions(&demo)?,
            "enemies" | "foes" => show_enemies(&demo)?,
            "regenerate" | "regen" => {
                if args.is_empty() {
                    println!("Usage: regenerate <actor name>");
                    println!("       regenerate \"Actor\" \"Incarnation\"");
                    println!("\nExamples:");
                    println!("  regenerate David Tennant");
                    println!("  regenerate \"Jodie Whittaker\" \"Thirteenth Doctor\"");
                } else {
                    // Check for quoted strings first: "Actor" "Incarnation"
                    let quotes: Vec<_> = args.match_indices('"').collect();
                    if quotes.len() >= 4 {
                        // At least 2 pairs of quotes
                        let actor = &args[quotes[0].0 + 1..quotes[1].0];
                        let incarnation = &args[quotes[2].0 + 1..quotes[3].0];
                        update_doctor_incarnation(&mut demo, actor, incarnation)?;
                    } else if quotes.len() >= 2 {
                        // Just one quoted string - use as actor
                        let actor = &args[quotes[0].0 + 1..quotes[1].0];
                        update_doctor_incarnation(&mut demo, actor, "Next Doctor")?;
                    } else {
                        // No quotes - entire argument is the actor name
                        let actor = args.trim();
                        update_doctor_incarnation(&mut demo, actor, "Next Doctor")?;
                    }
                }
            }
            "stats" | "stat" => show_stats(&demo)?,
            "timeline" | "history" | "tl" => show_timeline(&demo),
            "timewarp" | "tw" | "asof" => {
                if args.is_empty() {
                    println!("Usage: timewarp <incarnation_number>");
                    println!("Example: timewarp 1");
                    println!("\nUse 'timeline' to see available incarnation numbers.");
                } else if let Ok(num) = args.parse::<usize>() {
                    timewarp_query(&demo, num)?;
                } else {
                    println!("Please provide a valid incarnation number.");
                    println!("Use 'timeline' to see available incarnations.");
                }
            }
            _ => {
                println!(
                    "Unknown command: {}. Type 'help' for available commands.",
                    command
                );
            }
        }
    }

    Ok(())
}
