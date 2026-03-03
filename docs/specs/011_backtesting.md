# 🔭 Vantage Spec: Backtesting (The "Temporal Evaluation" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-011 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/query/` |

## 1. 👤 User Stories

> **As a** Trader,
> **I want to** backtest against volatile markets,
> **So that** I can evaluate trading strategies on historical price points and market events without affecting live data, ensuring resilience and profitability.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB stores historical data effectively through its bi-temporal features. However, currently, users are forced to write complex and slow loops in the application layer if they want to evaluate a logical scenario over a series of historical states. This lack of built-in tooling forces users to pull raw data over the network and process it externally, degrading the core value proposition of a bi-temporal database.

**The Gap:**
- **External Evaluation:** Time-series style backtesting happens entirely outside the database engine. Users must make thousands of individual point-in-time queries (e.g., `AS OF`) to evaluate a strategy.
- **Performance Overhead:** The network latency and application-side processing make large-scale backtesting impractically slow.
- **Inconsistent Error Handling:** External backtesting scripts often choke on missing or corrupted data points (e.g., NaN values), causing the entire evaluation run to fail.

**ROI:**
- **Product Stickiness:** Embedded strategy evaluation keeps computational workflows inside AletheiaDB, establishing it as a primary platform for temporal analytics, not just storage.
- **Competitive Advantage:** Fills a critical gap for quantitative finance, logistics, and machine learning use-cases that depend on rapid, reliable historical simulations.
- **Enhanced Data Integrity:** By centralizing the evaluation logic, we can guarantee predictable handling of edge cases (like NaN).

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Metric Definition:**
    -   Success = Backtest evaluation over 10,000 historical states completes in < 5 seconds for 99% of requests.

2.  **Iterative Evaluation:**
    -   Must be able to execute a given query or analytical function iteratively over a predefined range of historical states (Valid Time range with a specific interval/step size).
2.  **Resilience to Bad Data:**
    -   Must handle NaN data points or missing properties gracefully without panicking or halting the execution.
    -   Missing or invalid data should result in null evaluations for that specific time step, allowing the overall backtest to complete.
3.  **Reporting Output:**
    -   Must compile the evaluation results into a structured format.
    -   Must output a CSV report detailing the evaluation outcome at each time step.
4.  **Integration:**
    -   The backtesting engine must integrate with the existing Query Builder and AQL parsing infrastructure.

### Non-Functional Requirements
-   **Performance:** The internal execution loop must be highly optimized, avoiding repeated full query parsing for each time step. The cost model should reflect an efficient delta-stepping approach where possible.
-   **Usability:** The syntax or API for initiating a backtest must be declarative and intuitive.

## 4. 🚫 Out of Scope (Phase 1)

-   Real-time execution (Phase 2): This specification focuses strictly on historical evaluation, not live strategy deployment or triggering against incoming streams.
-   Complex multi-agent simulations: Initial scope is limited to deterministic query evaluation over time.
-   Direct integration with external trading APIs or brokers.