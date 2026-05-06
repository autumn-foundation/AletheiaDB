# Why AletheiaDB

Most databases answer one question: **what is true right now?**

AletheiaDB answers three: what is true now, what *was* true at any point in the
past, and what is *semantically related* — all in a single query, over a graph
of connected entities.

This sounds like a feature list. It isn't. Here's the problem it actually solves.

---

## The scenario

You're building an AI assistant for a healthcare provider. The assistant helps
clinicians answer questions like:

> "What treatment options have we used for patients with presentations similar
> to this one, and what do we know about their outcomes?"

That question has three parts:

1. **Relationships** — treatments are connected to patients, patients are
   connected to diagnoses, diagnoses are connected to protocols. This is a
   graph traversal problem.
2. **Similarity** — "similar to this one" means semantically similar: same
   cluster of symptoms, comorbidities, demographic factors. This is a vector
   search problem.
3. **History** — "what do we know" has to be anchored in time. Medical
   knowledge gets updated. A treatment that looked promising in 2022 may have
   been contraindicated in 2023 when new trial data arrived. A diagnosis may
   have been corrected after the fact. This is a temporal problem.

This is not a contrived edge case. Every serious knowledge application hits all
three of these eventually.

---

## What the obvious stack looks like

Most teams solve this by stitching together three systems:

```
Neo4j (graph) + Pinecone (vectors) + Postgres event tables (history)
```

It works. Until it doesn't.

**Consistency**: The three systems have no shared transaction boundary. If a
node is updated in Neo4j but the sync to Pinecone hasn't run yet, a query
that crosses both systems sees an inconsistent snapshot. You don't know which
version of the world you're looking at.

**Point-in-time queries**: Reconstructing "what did the system know on
March 15, 2023?" requires replaying events from Postgres, cross-referencing
with whatever state Neo4j had at that moment, and hoping the audit log is
complete. In practice, most teams give up and just answer "what does it say
now."

**Cross-dimensional queries**: "Traverse the graph, filter by time, rank by
similarity" requires executing three separate queries, loading the results
into memory, joining them by ID, and then ranking. The query planner in your
application code is not as good as a purpose-built query planner in the
database.

**Complexity tax**: Three systems means three schemas to keep in sync, three
failure modes to handle, three sets of credentials and connection pools, and
three mental models to hold simultaneously when something goes wrong at 2am.

---

## What changes with AletheiaDB

The same question — *what treatments have worked for similar patients?* —
becomes a single query:

```rust
// "What treatments did patients with similar presentations receive,
//  as of the treatment date — before any subsequent knowledge updates?"
let results = db.query()
    .as_of(treatment_date, treatment_date)      // temporal: exact knowledge state
    .start(patient_id)                          // graph: this patient
    .traverse("HAS_DIAGNOSIS")                  // graph: their diagnoses
    .traverse("TREATED_WITH")                   // graph: treatments used
    .rank_by_similarity(&symptom_embedding, 20) // semantic: similar presentations
    .filter(Predicate::eq("outcome", "positive"))
    .execute(&db)?;
```

This is not shorthand for "run three queries and join them." It's a single
consistent snapshot of the graph, evaluated at a specific point in valid time
and transaction time, with vector ranking applied over the traversal results.

---

## Why the temporal dimension is non-negotiable

The thing people underestimate is that **all knowledge has a provenance
problem**.

A drug interaction that wasn't in the database in 2022 is now. A relationship
between two entities that looked causal turned out to be correlational. A
diagnosis was revised. In a regular database, the correction silently
overwrites the past. You lose the ability to ask "what was known when the
decision was made?"

This matters for:
- **Compliance and audit**: Regulators often need to see what the system
  knew at the time of a decision, not what it knows now.
- **LLM reasoning**: A language model reasoning about historical events
  needs to be grounded in what was known then, not what is known now.
  Otherwise it will confidently reason from future knowledge about past
  decisions.
- **Debugging knowledge errors**: When an AI makes a bad recommendation,
  you need to reconstruct the exact knowledge state it was operating from.
  "The data looks fine now" is not useful.

AletheiaDB tracks two clocks independently:

- **Valid time**: When was this fact true in the real world?
- **Transaction time**: When did we record it in the database?

A diagnosis corrected in March can be queried as "what did the record show in
January" (transaction time) and separately as "what was actually true in
January" (valid time). These are different questions and they have different
answers.

---

## Why the semantic dimension completes the picture

Graph traversal finds things that are *connected*. Vector search finds things
that are *similar*. These overlap but are not the same.

Two patients may have similar symptom profiles without having any graph
connection. Two documents may share an author without being semantically
related. The graph tells you about explicit, recorded relationships; vectors
tell you about latent similarity in the underlying meaning.

The combination is particularly powerful for LLM applications: you can
traverse the graph to find related entities, then re-rank by semantic
relevance to the user's query, all within a single temporal snapshot. The
result is context that is structurally relevant *and* semantically relevant
*and* historically accurate.

---

## The alternative, in concrete terms

Here's the same query as a three-system implementation:

```python
# 1. Graph query (Neo4j)
with neo4j_driver.session() as s:
    graph_results = s.run("""
        MATCH (p:Patient {id: $id})-[:HAS_DIAGNOSIS]->(d)-[:TREATED_WITH]->(t)
        RETURN t.id, t.name
    """, id=patient_id).data()

treatment_ids = [r["t.id"] for r in graph_results]

# 2. Historical filter — did this treatment exist in the record on treatment_date?
# (requires a separate audit log table, custom replay logic, and hope
#  that every update was captured)
valid_treatments = filter_by_historical_state(treatment_ids, as_of=treatment_date)

# 3. Vector ranking (Pinecone)
pinecone_results = index.query(
    vector=symptom_embedding,
    filter={"id": {"$in": valid_treatments}},
    top_k=20
)

# 4. Fetch full records and join
final_results = []
for match in pinecone_results.matches:
    record = fetch_from_neo4j(match.id)         # another round trip
    if record.get("outcome") == "positive":
        final_results.append((record, match.score))

final_results.sort(key=lambda x: x[1], reverse=True)
```

This is not a strawman. This is approximately what production implementations
look like. It has at least four points of possible inconsistency, no shared
transaction semantics, no guaranteed point-in-time accuracy, and quadratic
complexity in the number of cross-system round trips.

It also has to be maintained, tested, and debugged by someone.

---

## When AletheiaDB is the right choice

AletheiaDB is the right tool when your application needs to answer questions
that cross all three dimensions — graph structure, semantic meaning, and
historical accuracy — and when inconsistency between those dimensions is
unacceptable.

**Strong fits:**
- LLM applications that need to reason about knowledge that changes over time
- Compliance systems that must reconstruct historical state for audit
- Knowledge graphs where entities have embeddings and evolve over time
- RAG pipelines where retrieved context must be temporally grounded
- Research tools tracking how understanding of a domain has shifted

**Weaker fits:**
- Pure key-value lookups with no relational structure
- Applications that genuinely only care about current state and have no
  need for history
- Workloads where the graph, vector, and temporal dimensions never intersect

---

## Next step

→ [Core Concepts](core-concepts.md) — the precise mechanics of valid time,
transaction time, nodes, edges, and how queries work.
