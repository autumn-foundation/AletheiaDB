# 🔭 Vantage: Spec for Backtesting Engine

**Status**: Draft
**Owner**: Vantage
**Date**: 2024-05-22

## 1. Executive Summary

This specification outlines the requirements for a native backtesting engine within AletheiaDB.

## 2. User Stories

### 2.1 The Trader
👤 **User Story:** "As a Trader, I want to backtest against volatile markets using historical knowledge graphs, so that I can simulate trading strategies exactly as the market structure existed at that point in time."

## 3. Problem Statement

Currently, users can query the historical state of the graph, but they must manually iterate through time windows in application logic to simulate event streams.

**What business problem does this solve?**
Performance: Manually stepping through time (`AS OF t1`, then `AS OF t2`) is slow because the query engine re-evaluates everything from scratch.
Developer Experience (DX): Building event-driven simulators requires orchestrating a lot of boilerplate code instead of running a single declarative backtest.

## 4. Proposed Solution

A built-in backtesting engine leveraging our native bi-temporal capabilities.

## 5. Functional Requirements
✅ **Acceptance Criteria:**
- Must handle NaN data without panicking.
- Must output a CSV report summarizing the backtest results.

## 6. Non-Functional Requirements
- Metric Definition: Success = Query latency < 10ms for 99% of requests.

## 7. Out of Scope
🚫 **Out of Scope:** Real-time execution (Phase 2).
