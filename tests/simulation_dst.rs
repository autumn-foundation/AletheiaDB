//! Deterministic Simulation Testing (DST) integration tests (issue #154).
//!
//! These tests validate the simulation framework's core components:
//! - `SimulatedClock`: controllable timestamps for temporal scenario testing
//! - `FaultInjector`: deterministic fault injection for storage/crash scenarios
//! - `SimulatedStorage`: in-memory storage with configurable faults
//! - `SimulatedScheduler`: deterministic task interleaving
//! - `Simulator`: the top-level test harness

#[cfg(feature = "simulation")]
mod dst_tests {
    use aletheiadb::core::temporal::time;
    use aletheiadb::simulation::{
        FaultConfig, FaultType, SimulatedClock, SimulatedScheduler, SimulatedStorage, Simulator,
    };
    use aletheiadb::{PropertyMapBuilder, WriteOps};

    // =========================================================================
    // SimulatedClock tests
    // =========================================================================

    #[test]
    fn simulated_clock_starts_at_configured_time() {
        let clock = SimulatedClock::new(1_000_000); // 1 second in µs
        assert_eq!(clock.now_micros(), 1_000_000);
    }

    #[test]
    fn simulated_clock_advance_by_increases_time() {
        let mut clock = SimulatedClock::new(0);
        clock.advance_by(500);
        assert_eq!(clock.now_micros(), 500);
    }

    #[test]
    fn simulated_clock_multiple_advances_accumulate() {
        let mut clock = SimulatedClock::new(100);
        clock.advance_by(50);
        clock.advance_by(75);
        assert_eq!(clock.now_micros(), 225);
    }

    #[test]
    fn simulated_clock_jump_to_arbitrary_time() {
        let mut clock = SimulatedClock::new(1_000_000);
        clock.jump_to(500_000); // Jump backwards
        assert_eq!(clock.now_micros(), 500_000);
    }

    #[test]
    fn simulated_clock_jump_backwards_is_allowed() {
        let mut clock = SimulatedClock::new(1_000);
        clock.jump_to(0);
        assert_eq!(clock.now_micros(), 0);
    }

    #[test]
    fn simulated_clock_now_as_timestamp_matches_current_micros() {
        let clock = SimulatedClock::new(2_000_000);
        let ts = clock.now_as_timestamp();
        assert_eq!(ts.wallclock(), 2_000_000);
    }

    #[test]
    fn simulated_clock_inject_overrides_time_now() {
        let clock = SimulatedClock::new(42_000_000);
        let _guard = clock.inject();
        let ts = time::now();
        assert_eq!(ts.wallclock(), 42_000_000);
    }

    #[test]
    fn simulated_clock_guard_restores_real_clock_on_drop() {
        let real_before = time::now().wallclock();
        {
            let clock = SimulatedClock::new(1); // far past
            let _guard = clock.inject();
            assert_eq!(time::now().wallclock(), 1);
        }
        // After guard drops, time::now() should be close to wall clock again
        let real_after = time::now().wallclock();
        // Real clock should be >= the time we captured before (monotonic)
        assert!(real_after >= real_before);
    }

    // =========================================================================
    // FaultConfig / FaultInjector tests
    // =========================================================================

    #[test]
    fn fault_config_default_has_no_faults() {
        let cfg = FaultConfig::default();
        assert_eq!(cfg.corruption_rate, 0.0);
        assert!(cfg.crash_at_byte_offset.is_none());
    }

    #[test]
    fn fault_config_with_corruption_rate() {
        let cfg = FaultConfig::default().with_corruption_rate(0.5);
        assert_eq!(cfg.corruption_rate, 0.5);
    }

    #[test]
    fn fault_config_with_crash_offset() {
        let cfg = FaultConfig::default().with_crash_at_offset(1024);
        assert_eq!(cfg.crash_at_byte_offset, Some(1024));
    }

    #[test]
    fn fault_injector_no_corruption_when_rate_zero() {
        let cfg = FaultConfig::default().with_corruption_rate(0.0);
        let sim = Simulator::with_seed_and_faults(42, cfg);
        let mut data = vec![0xAB_u8; 64];
        let original = data.clone();
        sim.faults().try_corrupt(&mut data);
        assert_eq!(data, original, "Zero corruption rate must not alter data");
    }

    #[test]
    fn fault_injector_always_corrupts_when_rate_one() {
        let cfg = FaultConfig::default().with_corruption_rate(1.0);
        let sim = Simulator::with_seed_and_faults(42, cfg);
        let mut data = vec![0x00_u8; 64];
        sim.faults().try_corrupt(&mut data);
        // At 100% corruption rate at least one byte must differ
        assert!(
            data.iter().any(|&b| b != 0x00),
            "Full corruption rate must alter data"
        );
    }

    #[test]
    fn fault_injector_crash_at_byte_offset_triggers_when_reached() {
        let cfg = FaultConfig::default().with_crash_at_offset(100);
        let sim = Simulator::with_seed_and_faults(42, cfg);
        // Before the offset — no crash
        assert!(!sim.faults().should_crash_at(50));
        // At the offset — crash
        assert!(sim.faults().should_crash_at(100));
        // After the offset — crash (once past threshold)
        assert!(sim.faults().should_crash_at(200));
    }

    #[test]
    fn fault_injector_no_crash_when_no_offset_configured() {
        let cfg = FaultConfig::default(); // no crash offset
        let sim = Simulator::with_seed_and_faults(42, cfg);
        assert!(!sim.faults().should_crash_at(0));
        assert!(!sim.faults().should_crash_at(u64::MAX));
    }

    #[test]
    fn fault_injector_is_deterministic_with_same_seed() {
        let cfg = FaultConfig::default().with_corruption_rate(0.5);

        let sim_a = Simulator::with_seed_and_faults(7, cfg.clone());
        let sim_b = Simulator::with_seed_and_faults(7, cfg.clone());

        let mut data_a = vec![0xFF_u8; 32];
        let mut data_b = data_a.clone();
        sim_a.faults().try_corrupt(&mut data_a);
        sim_b.faults().try_corrupt(&mut data_b);

        assert_eq!(
            data_a, data_b,
            "Same seed must produce same corruption pattern"
        );
    }

    #[test]
    fn fault_injector_different_seeds_produce_different_results() {
        let cfg = FaultConfig::default().with_corruption_rate(0.5);

        let sim_a = Simulator::with_seed_and_faults(1, cfg.clone());
        let sim_b = Simulator::with_seed_and_faults(2, cfg.clone());

        // Run many rounds to make collision extremely unlikely
        let mut any_differ = false;
        for _ in 0..20 {
            let mut da = vec![0x00_u8; 32];
            let mut db = da.clone();
            sim_a.faults().try_corrupt(&mut da);
            sim_b.faults().try_corrupt(&mut db);
            if da != db {
                any_differ = true;
                break;
            }
        }
        assert!(
            any_differ,
            "Different seeds should produce different corruption"
        );
    }

    // =========================================================================
    // SimulatedStorage tests
    // =========================================================================

    #[test]
    fn simulated_storage_write_and_read_roundtrip() {
        let mut storage = SimulatedStorage::new();
        storage.write("key1", b"hello world").unwrap();
        let read = storage.read("key1").expect("key must be present");
        assert_eq!(read, b"hello world");
    }

    #[test]
    fn simulated_storage_read_missing_key_returns_none() {
        let storage = SimulatedStorage::new();
        assert!(storage.read("nonexistent").is_none());
    }

    #[test]
    fn simulated_storage_tracks_bytes_written() {
        let mut storage = SimulatedStorage::new();
        storage.write("a", &[1, 2, 3]).unwrap();
        storage.write("b", &[4, 5]).unwrap();
        assert_eq!(storage.bytes_written(), 5);
    }

    #[test]
    fn simulated_storage_crash_at_offset_blocks_writes_past_threshold() {
        let cfg = FaultConfig::default().with_crash_at_offset(5);
        let mut storage = SimulatedStorage::with_faults(cfg, 0);
        // First write (3 bytes at offset 0): should succeed
        storage.write("k1", &[1, 2, 3]).unwrap();
        // Second write pushes past byte 5: should return an error (simulated crash)
        let result = storage.write("k2", &[4, 5, 6]);
        assert!(result.is_err(), "Write past crash offset must fail");
    }

    #[test]
    fn simulated_storage_overwrite_existing_key() {
        let mut storage = SimulatedStorage::new();
        storage.write("k", b"v1").unwrap();
        storage.write("k", b"v2").unwrap();
        assert_eq!(storage.read("k").unwrap(), b"v2");
    }

    // =========================================================================
    // SimulatedScheduler tests
    // =========================================================================

    #[test]
    fn simulated_scheduler_runs_all_tasks() {
        let mut sched = SimulatedScheduler::new(0);
        let results = sched.run_tasks(vec![
            Box::new(|| 1_u32),
            Box::new(|| 2_u32),
            Box::new(|| 3_u32),
        ]);
        let mut sorted = results.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3]);
    }

    #[test]
    fn simulated_scheduler_is_deterministic_with_same_seed() {
        let tasks_a: Vec<Box<dyn FnOnce() -> usize>> =
            vec![Box::new(|| 10), Box::new(|| 20), Box::new(|| 30)];
        let tasks_b: Vec<Box<dyn FnOnce() -> usize>> =
            vec![Box::new(|| 10), Box::new(|| 20), Box::new(|| 30)];
        let mut sched_a = SimulatedScheduler::new(99);
        let mut sched_b = SimulatedScheduler::new(99);
        let order_a = sched_a.run_tasks(tasks_a);
        let order_b = sched_b.run_tasks(tasks_b);
        assert_eq!(
            order_a, order_b,
            "Same seed must produce same execution order"
        );
    }

    #[test]
    fn simulated_scheduler_empty_task_list_returns_empty() {
        let mut sched = SimulatedScheduler::new(0);
        let results: Vec<i32> = sched.run_tasks(vec![]);
        assert!(results.is_empty());
    }

    // =========================================================================
    // Simulator integration tests
    // =========================================================================

    #[test]
    fn simulator_new_creates_with_seed() {
        let sim = Simulator::new(12345);
        assert_eq!(sim.seed(), 12345);
    }

    #[test]
    fn simulator_clock_starts_at_epoch() {
        let sim = Simulator::new(0);
        // New simulator clock starts at time 0
        assert_eq!(sim.clock().now_micros(), 0);
    }

    #[test]
    fn simulator_advance_time_moves_clock() {
        let mut sim = Simulator::new(0);
        sim.advance_time_by(100_000);
        assert_eq!(sim.clock().now_micros(), 100_000);
    }

    #[test]
    fn simulator_inject_clock_jump_backwards() {
        let mut sim = Simulator::new(0);
        sim.advance_time_by(1_000_000);
        sim.inject_clock_jump(-500_000);
        assert_eq!(sim.clock().now_micros(), 500_000);
    }

    #[test]
    fn simulator_inject_clock_jump_forwards() {
        let mut sim = Simulator::new(0);
        sim.inject_clock_jump(2_000_000);
        assert_eq!(sim.clock().now_micros(), 2_000_000);
    }

    #[test]
    fn simulator_verify_temporal_invariants_on_fresh_db() {
        let sim = Simulator::new(42);
        let (_temp, db) = aletheiadb::test_utils::create_test_db().unwrap();
        // A fresh DB with no data must pass all temporal invariants
        let report = sim.verify_temporal_invariants(&db);
        assert!(
            report.passed,
            "Fresh DB must satisfy temporal invariants: {:?}",
            report.violations
        );
    }

    #[test]
    fn simulator_verify_temporal_invariants_after_node_operations() {
        let sim = Simulator::new(1);
        let (_temp, db) = aletheiadb::test_utils::create_test_db().unwrap();

        // Create some nodes
        let props = PropertyMapBuilder::new().build();
        let n1 = db.create_node("Person", props.clone()).unwrap();
        let n2 = db.create_node("Person", props.clone()).unwrap();

        // Update a node (creates a new version)
        db.write(|tx: &mut aletheiadb::WriteTransaction| {
            tx.update_node(n1, PropertyMapBuilder::new().insert("age", 30_i64).build())
        })
        .unwrap();

        // Delete one
        db.write(|tx: &mut aletheiadb::WriteTransaction| tx.delete_node(n2))
            .unwrap();

        let report = sim.verify_temporal_invariants(&db);
        assert!(
            report.passed,
            "DB after create/update/delete must satisfy temporal invariants: {:?}",
            report.violations
        );
    }

    #[test]
    fn simulator_temporal_invariants_report_contains_node_count() {
        let sim = Simulator::new(0);
        let (_temp, db) = aletheiadb::test_utils::create_test_db().unwrap();
        let props = PropertyMapBuilder::new().build();
        db.create_node("X", props.clone()).unwrap();
        db.create_node("X", props.clone()).unwrap();
        let report = sim.verify_temporal_invariants(&db);
        assert_eq!(report.nodes_checked, 2);
    }

    // =========================================================================
    // End-to-end scenario: clock jump during DB operations
    // =========================================================================

    #[test]
    fn scenario_clock_jump_does_not_corrupt_historical_storage() {
        let mut sim = Simulator::new(1001);
        let (_temp, db) = aletheiadb::test_utils::create_test_db().unwrap();
        let props = PropertyMapBuilder::new().build();

        // Create node at simulated time t=100s
        sim.advance_time_by(100_000_000); // 100 seconds in µs
        let node = db.create_node("Event", props.clone()).unwrap();

        // Update the node at t=110s (valid: 110 > 100)
        sim.advance_time_by(10_000_000);
        db.write(|tx: &mut aletheiadb::WriteTransaction| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("step", 1_i64).build(),
            )
        })
        .unwrap();

        // Inject a backwards clock jump (simulate NTP correction).
        // The jump takes us to t=90s. New entities created at t=90s are fine;
        // updating existing entities (created at t=100s) at t<100s would be
        // correctly rejected by the DB — that is the invariant being enforced.
        sim.inject_clock_jump(-20_000_000); // jump back 20 seconds → t=90s

        // Create a *new* node at t=90s: this is valid (fresh entity).
        db.create_node("Event", props.clone()).unwrap();

        // Updating the first node at t=90s < creation-time (100s) must be
        // rejected — the DB is correctly enforcing temporal ordering.
        let backward_update_result = db.write(|tx: &mut aletheiadb::WriteTransaction| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("processed", true).build(),
            )
        });
        assert!(
            backward_update_result.is_err(),
            "Update before entity creation time must be rejected"
        );

        // Despite the rejected write, the successfully-written data must still
        // satisfy all temporal invariants.
        let report = sim.verify_temporal_invariants(&db);
        assert!(
            report.passed,
            "Temporal invariants violated after clock jump: {:?}",
            report.violations
        );
    }

    // =========================================================================
    // End-to-end scenario: fault injection on simulated storage
    // =========================================================================

    #[test]
    fn scenario_fault_injected_storage_detects_crash_boundary() {
        let cfg = FaultConfig::default().with_crash_at_offset(10);
        let mut storage = SimulatedStorage::with_faults(cfg, 42);

        // Small write — succeeds
        assert!(storage.write("first", &[0u8; 5]).is_ok());
        // Second write crosses the crash boundary
        let result = storage.write("second", &[0u8; 10]);
        assert!(result.is_err());
        // Bytes written reflects only what succeeded
        assert!(storage.bytes_written() <= 10);
    }

    // =========================================================================
    // End-to-end scenario: deterministic reproduction
    // =========================================================================

    #[test]
    fn scenario_same_seed_reproduces_identical_fault_sequence() {
        let cfg = FaultConfig::default().with_corruption_rate(0.3);

        let run = |seed: u64| -> Vec<Vec<u8>> {
            let sim = Simulator::with_seed_and_faults(seed, cfg.clone());
            (0..10_u8)
                .map(|i| {
                    let mut data = vec![i; 16];
                    sim.faults().try_corrupt(&mut data);
                    data
                })
                .collect()
        };

        let run_a = run(77);
        let run_b = run(77);
        assert_eq!(
            run_a, run_b,
            "Same seed must reproduce identical fault sequence"
        );
    }

    // =========================================================================
    // FaultType enum sanity
    // =========================================================================

    #[test]
    fn fault_type_variants_are_accessible() {
        let _ = FaultType::StorageCorruption;
        let _ = FaultType::StorageLatency { micros: 100 };
        let _ = FaultType::CrashAtOffset { byte_offset: 512 };
        let _ = FaultType::ClockJump {
            delta_micros: -1000,
        };
    }
}
