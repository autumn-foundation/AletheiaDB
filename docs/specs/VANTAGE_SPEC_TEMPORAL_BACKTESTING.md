# 🔭 Vantage: Spec for Temporal Backtesting

👤 **User Story:**
As a Trader, I want to backtest against volatile markets using historical graph data, so that I can evaluate the performance of my trading strategies over time without risking real capital.

🧐 **The "So What?" (Business Value):**
Currently, traders lack the ability to replay historical market conditions dynamically within the graph to see how their strategies would have performed. By building temporal backtesting, we allow them to simulate trades against actual historical data, leading to higher confidence in strategy deployment and better risk management.

**Gap Analysis:**
Existing solutions either focus solely on static time-series data without relational context, or they require extracting data from the database into Python pandas/NumPy for simulation. AletheiaDB lacks a built-in function to traverse the graph chronologically while simulating a trading state machine. Building this natively will eliminate the network overhead and simplify the trading data pipeline.

**Metric Definition:**
- Success = Backtesting a 1-year historical window with 10M events completes in < 5 seconds.

✅ **Acceptance Criteria:**
- Must handle NaN data without panicking.
- Must output a CSV report summarizing the backtest results.
- Must accurately replay historical graph states over a defined valid time range.

🚫 **Out of Scope:**
- Real-time execution (Phase 2).
