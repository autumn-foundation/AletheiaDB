use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aletheiadb::core::{PropertyMapBuilder, interning::GLOBAL_INTERNER, temporal::time};
use aletheiadb::storage::wal::{
    DurabilityMode, WalOperation,
    concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
};

#[derive(Debug, Clone)]
struct ProfileConfig {
    threads: usize,
    ops_per_thread: usize,
    warmup_ops: usize,
    max_delay_ms: u64,
    max_batch_size: usize,
    trigger_threshold: Option<usize>,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            threads: 8,
            ops_per_thread: 500,
            warmup_ops: 100,
            max_delay_ms: 10,
            max_batch_size: 20,
            trigger_threshold: None,
        }
    }
}

fn parse_args() -> ProfileConfig {
    let mut cfg = ProfileConfig::default();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1usize;
    while i + 1 < args.len() {
        let key = &args[i];
        let value = &args[i + 1];
        match key.as_str() {
            "--threads" => {
                if let Ok(v) = value.parse::<usize>() {
                    cfg.threads = v.max(1);
                }
            }
            "--ops" => {
                if let Ok(v) = value.parse::<usize>() {
                    cfg.ops_per_thread = v.max(1);
                }
            }
            "--warmup" => {
                if let Ok(v) = value.parse::<usize>() {
                    cfg.warmup_ops = v;
                }
            }
            "--delay-ms" => {
                if let Ok(v) = value.parse::<u64>() {
                    cfg.max_delay_ms = v.max(1);
                }
            }
            "--batch-size" => {
                if let Ok(v) = value.parse::<usize>() {
                    cfg.max_batch_size = v.max(1);
                }
            }
            "--trigger-threshold" => {
                if let Ok(v) = value.parse::<usize>() {
                    cfg.trigger_threshold = Some(v.max(1));
                }
            }
            _ => {}
        }
        i += 2;
    }
    cfg
}

fn percentile_ns(sorted_ns: &[u64], pct: f64) -> u64 {
    if sorted_ns.is_empty() {
        return 0;
    }
    let max_index = sorted_ns.len() - 1;
    let rank = ((pct * max_index as f64).ceil() as usize).min(max_index);
    sorted_ns[rank]
}

fn mean_ns(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let sum: u128 = values.iter().map(|&v| v as u128).sum();
    (sum as f64) / (values.len() as f64)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = parse_args();
    println!("group_commit_wait_profile config: {:?}", cfg);

    let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let wal_dir = std::env::temp_dir().join(format!("aletheiadb-gc-profile-{}", ts));
    fs::create_dir_all(&wal_dir)?;

    let mut wal_config = ConcurrentWalSystemConfig::new(&wal_dir)
        .with_durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: cfg.max_delay_ms,
            max_batch_size: cfg.max_batch_size,
        })
        .with_flush_interval_ms(cfg.max_delay_ms);
    if let Some(trigger_threshold) = cfg.trigger_threshold {
        wal_config = wal_config.with_group_commit_flush_trigger_batch_size(trigger_threshold);
    }
    let wal = Arc::new(ConcurrentWalSystem::new(wal_config)?);

    wal.set_group_commit_metrics_enabled(true);
    if let Some(gc) = wal.group_commit_coordinator() {
        gc.reset_metrics();
        println!(
            "group_commit_wait_profile trigger threshold: {}",
            gc.flush_trigger_batch_size()
        );
    }

    // Warmup to reduce one-time startup effects.
    for i in 0..cfg.warmup_ops {
        let op = WalOperation::CreateNode {
            node_id: aletheiadb::core::id::NodeId::new((i + 1) as u64)?,
            label: GLOBAL_INTERNER.intern("Warmup")?,
            properties: PropertyMapBuilder::new().insert("warmup", i as i64).build(),
            valid_from: time::now(),
        };
        wal.append_async(op)?;
        wal.commit_group_commit_and_wait()?;
    }

    wal.reset_group_commit_metrics();

    let latencies = Arc::new(Mutex::new(Vec::with_capacity(
        cfg.threads * cfg.ops_per_thread,
    )));

    let start_total = Instant::now();
    let mut handles = Vec::with_capacity(cfg.threads);
    for thread_id in 0..cfg.threads {
        let wal = Arc::clone(&wal);
        let latencies = Arc::clone(&latencies);
        handles.push(thread::spawn(move || -> Result<(), String> {
            let base = (thread_id as u64) * 1_000_000_000;
            for i in 0..cfg.ops_per_thread {
                let op = WalOperation::CreateNode {
                    node_id: aletheiadb::core::id::NodeId::new(base + i as u64 + 1)
                        .map_err(|e| e.to_string())?,
                    label: GLOBAL_INTERNER
                        .intern("Profile")
                        .map_err(|e| e.to_string())?,
                    properties: PropertyMapBuilder::new()
                        .insert("thread", thread_id as i64)
                        .insert("counter", i as i64)
                        .build(),
                    valid_from: time::now(),
                };
                wal.append_async(op).map_err(|e| e.to_string())?;
                let start = Instant::now();
                wal.commit_group_commit_and_wait()
                    .map_err(|e| e.to_string())?;
                let elapsed_ns = start.elapsed().as_nanos() as u64;
                latencies
                    .lock()
                    .map_err(|_| "latencies lock poisoned".to_string())?
                    .push(elapsed_ns);
            }
            Ok(())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "worker thread panicked")?
            .map_err(|e| format!("worker failed: {}", e))?;
    }
    let elapsed_total = start_total.elapsed();

    let mut latencies_ns = latencies
        .lock()
        .map_err(|_| "latencies lock poisoned")?
        .clone();
    latencies_ns.sort_unstable();

    let p50_ns = percentile_ns(&latencies_ns, 0.50);
    let p95_ns = percentile_ns(&latencies_ns, 0.95);
    let p99_ns = percentile_ns(&latencies_ns, 0.99);
    let avg_ns = mean_ns(&latencies_ns);
    let max_ns = latencies_ns.last().copied().unwrap_or(0);
    let total_ops = latencies_ns.len() as f64;
    let throughput = if elapsed_total > Duration::ZERO {
        total_ops / elapsed_total.as_secs_f64()
    } else {
        0.0
    };

    println!(
        "commit_wait latency (us): count={} p50={:.2} p95={:.2} p99={:.2} avg={:.2} max={:.2}",
        latencies_ns.len(),
        p50_ns as f64 / 1_000.0,
        p95_ns as f64 / 1_000.0,
        p99_ns as f64 / 1_000.0,
        avg_ns / 1_000.0,
        max_ns as f64 / 1_000.0
    );
    println!(
        "workload: duration={:.3}s throughput={:.1} commits/sec",
        elapsed_total.as_secs_f64(),
        throughput
    );

    if let Some(metrics) = wal.group_commit_metrics_snapshot() {
        let avg_state_lock_wait_ns = if metrics.state_lock_acquires > 0 {
            metrics.state_lock_wait_ns as f64 / metrics.state_lock_acquires as f64
        } else {
            0.0
        };
        let avg_waiters_lock_wait_ns = if metrics.waiters_lock_acquires > 0 {
            metrics.waiters_lock_wait_ns as f64 / metrics.waiters_lock_acquires as f64
        } else {
            0.0
        };
        let avg_commit_wait_ns = if metrics.wait_calls > 0 {
            metrics.wait_total_ns as f64 / metrics.wait_calls as f64
        } else {
            0.0
        };

        println!("group_commit metrics:");
        println!(
            "  locks: state(acquires={}, avg_wait_ns={:.1}) waiters(acquires={}, avg_wait_ns={:.1})",
            metrics.state_lock_acquires,
            avg_state_lock_wait_ns,
            metrics.waiters_lock_acquires,
            avg_waiters_lock_wait_ns
        );
        println!(
            "  waits: calls={} success={} timeouts={} errors={} avg_wait_ns={:.1}",
            metrics.wait_calls,
            metrics.wait_successes,
            metrics.wait_timeouts,
            metrics.wait_errors,
            avg_commit_wait_ns
        );
        println!(
            "  lifecycle: waiter_created={} waiter_removed={} trigger_flush_calls={} epoch_completions={}",
            metrics.waiter_created,
            metrics.waiter_removed,
            metrics.trigger_flush_calls,
            metrics.epoch_completions
        );
    }

    let _ = fs::remove_dir_all(&wal_dir);
    Ok(())
}
