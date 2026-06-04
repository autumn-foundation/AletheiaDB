# 🔭 Vantage: Spec for Temporal Backtesting

👤 **User Story:** "As a Trader, I want to backtest against volatile markets, so that I can validate my algorithms without lookahead bias."

🧐 **The "So What?" ask: "What business problem does this solve?"**
Standard graph pathfinding or temporal queries are not enough for simulating sequential logic. Backtesting allows users to test algorithms over historical market graphs, identifying vulnerabilities and adjusting risk models based on accurate historical snapshots rather than post-event aggregated data.

**Gap Analysis:**
Standard libraries and existing graph databases often lack native bi-temporal capabilities combined with a robust framework for simulating processes over that history. Building this natively avoids manual, out-of-band, and error-prone client-side orchestration.

**Metric Definition:**
Success = Query latency < 10ms for 99% of requests.

✅ **Acceptance Criteria:**
- Must handle NaN data without panicking.
- Must output a CSV report.

🚫 **Out of Scope:**
- Real-time execution (Phase 2).
