# 40. Sherlock: Temporal Pattern Matching Engine

Date: 2024-05-22

## Status

Proposed

## Context

AletheiaDB stores the complete history of every entity, enabling powerful temporal analysis. However, standard graph queries (e.g., "Find neighbors of X at time T") are insufficient for detecting complex behavioral patterns or sequences of events.

Use cases like fraud detection, user journey analysis, and system diagnostics often require questions like:
- "Did a user change their status from 'Active' to 'Suspended' and then 'Deleted' within 10 minutes?"
- "Find all servers that reported 'High CPU' followed by 'Crash' within 5 seconds."

These queries involve:
1.  **Sequence Matching**: Finding an ordered list of states (A -> B -> C).
2.  **Temporal Constraints**: Enforcing a maximum time window for the entire sequence.
3.  **Historical Traversal**: Scanning the valid-time history of a single node.

Without a dedicated engine, users would have to fetch the entire history and implement complex filtering logic in their application code, leading to inefficiency and duplicated effort.

## Decision

We will implement **Sherlock**, a Temporal Pattern Matching Engine, as part of the `Nova` experimental suite.

### Core Concepts

1.  **Clue**: A specific condition to look for at a point in time (e.g., `status == "Error"`).
2.  **Mystery**: A definition of the pattern to find, consisting of an ordered list of Clues and a maximum `time_window`.
3.  **Deduction**: A concrete match found in the data, containing the node ID and the timestamps of each matched Clue.

### Algorithm

Sherlock operates on the `VersionHistory` of a node:

1.  **Fetch & Sort**: Retrieve the node's history and sort versions by `valid_time.start`. This ensures we process events in the order they occurred in the real world.
2.  **Anchor Search**: Iterate through the history to find all versions that match the first Clue. These are potential start points.
3.  **Forward Scan**: For each start point, search forward in the sorted history to find the subsequent Clues.
    -   **Constraint Check**: Ensure the time difference between the current candidate and the start point does not exceed the `time_window`.
    -   **Order Check**: Ensure the candidate's time is strictly greater than the previous match's time (if strict ordering is required).
4.  **Backtracking/Greedy**: The current implementation uses a greedy approach for subsequent clues (finding the first valid match). Future versions may support full backtracking to find all overlapping patterns.

## Consequences

### Positive
-   **Expressive Power**: Enables users to define and detect complex temporal patterns declaratively.
-   **Efficiency**: Pushes the pattern matching logic close to the data (within the database process), avoiding large data transfers.
-   **Modularity**: Sherlock is implemented as a separate module in `experimental`, using the public `get_node_history` API, keeping the core kernel clean.

### Negative
-   **Performance Overhead**: The current implementation performs a linear scan of the history. For nodes with millions of versions, this could be slow. Indexing strategies (e.g., temporal inverted indexes) may be needed for scaling.
-   **Memory Usage**: Fetching the full history into memory for sorting can be expensive for very volatile nodes.
-   **Scope Limitation**: Currently limited to single-node property patterns. Multi-node patterns (finding sequences involving neighbors) are not yet supported.
