# 🔭 Vantage: Spec for Temporal Backtesting

## 1. 👤 User Story

> **As a** Trader,
> **I want to** backtest against volatile markets,
> **So that** I can evaluate trading strategies without risking real capital.

## 2. 🧐 The "So What?" (Business Value)

Financial systems rely heavily on historical simulations to validate models. Using a standard database to reconstruct historical state is slow and often suffers from look-ahead bias.

**ROI:**
- **Market Expansion:** Unlocks quantitative finance use cases.
- **Trust:** Guarantees strict bitemporal isolation, eliminating look-ahead bias.

## 3. ✅ Acceptance Criteria

- Must handle NaN data without panicking.
- Must output a CSV report.
- Must provide a temporal stepper to advance time.

## 4. 🚫 Out of Scope

- Real-time execution (Phase 2).
