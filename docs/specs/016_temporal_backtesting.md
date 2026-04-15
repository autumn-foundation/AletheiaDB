# 🔭 Vantage: Spec for Temporal Backtesting Engine

## 👤 User Story
**As a** Trader or Quantitative Analyst,
**I want** to backtest algorithmic models against volatile historical graph states,
**so that** I can validate my trading strategies and fraud detection rules over time without risking live capital.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, users can query the historical state of the graph at a specific point in time using AletheiaDB's bi-temporal features (e.g., `AS OF` queries). However, there is no native engine to seamlessly "replay" or simulate a complex series of logic across an evolving graph timeline. This forces organizations to export massive datasets into separate data warehouses or backtesting frameworks, breaking the single source of truth and adding immense engineering overhead.

A native Temporal Backtesting Engine turns the database itself into a historical simulator, drastically reducing the Time-to-Market for deploying robust algorithmic models and directly converting our bi-temporal storage into actionable predictive value.

**Success Metric Definition:**
- **Simulation Latency:** Executing a backtest simulating 1 month of graph history (with at least 100,000 temporal state changes) completes in < 5 seconds.
- **Accuracy:** The simulated graph state at any tick `T` exactly matches the result of a direct `AS OF T` query.

## ✅ Acceptance Criteria
- Must define a high-level API to define a backtest simulation, accepting a `start_time`, `end_time`, and an optional `tick_interval` or `event_driven` mode.
- Must provide a callback or execution hook allowing user-defined logic (or AQL queries) to be evaluated at each simulated state.
- Must efficiently leverage the underlying temporal indices to stream state changes (deltas) rather than re-computing the entire graph at each tick.
- Must return an aggregated report (e.g., CSV or structured JSON) of the simulation's results and triggered events.
- Must handle missing data or missing vectors during historical periods gracefully without panicking.

## 🚫 Out of Scope
- Real-time execution against streaming live data (Phase 2).
- Direct integration with live trading or broker APIs (this is strictly a simulator).
- Cross-shard distributed simulations (Phase 2).
