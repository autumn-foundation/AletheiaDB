// Neo4j Cypher mappings for the same logical queries the AletheiaDB suite runs.
// NOT executed in the sandbox; provided so a third party can reproduce on Neo4j.
// Parameters ($personId, $postId, $forumId, $queryEmbedding) are supplied by the
// driver, cycling the same sampled ids the AletheiaDB runner uses.
//
// Temporal-extension queries are intentionally ABSENT: Neo4j has no native
// bi-temporal reconstruction (see incumbents/README.md, footnote 1). Emulating
// it via interval properties is a different, hand-built workload and is not
// presented as the same operation.

// ---- Short reads (IS) ----

// IS1 — person profile
MATCH (p:Person {id: $personId}) RETURN p;

// IS2 — messages authored by a person
MATCH (p:Person {id: $personId})<-[:HAS_CREATOR]-(m) RETURN m;

// IS3 — direct friends
MATCH (p:Person {id: $personId})-[:KNOWS]->(f:Person) RETURN f;

// IS5 — creator of a post
MATCH (post:Post {id: $postId})-[:HAS_CREATOR]->(c:Person) RETURN c;

// IS6 — forum of a post
MATCH (post:Post {id: $postId})<-[:CONTAINER_OF]-(forum:Forum) RETURN forum;

// ---- Complex reads (IC) ----

// IC1 — friends within 2 hops (node-distinct)
MATCH (p:Person {id: $personId})-[:KNOWS*1..2]->(f:Person)
WHERE f <> p RETURN DISTINCT f;

// IC2 — messages authored by direct friends
MATCH (p:Person {id: $personId})-[:KNOWS]->(f:Person)<-[:HAS_CREATOR]-(m)
RETURN m;

// IC9 — messages by friends-of-friends (2 hops)
MATCH (p:Person {id: $personId})-[:KNOWS*1..2]->(f:Person)<-[:HAS_CREATOR]-(m)
WHERE f <> p RETURN m;

// ---- Insert/update stream ----

// INS1 — insert person
CREATE (p:Person {id: $newId, name: $name, city: $city});

// INS6 — insert post + creator edge
MATCH (c:Person {id: $creatorId})
CREATE (post:Post {id: $newId, length: 100})-[:HAS_CREATOR]->(c);

// UPD — update person city
MATCH (p:Person {id: $personId}) SET p.city = $city;

// ---- Vector extension (Neo4j 5.x vector index; expressible, NOT measured here) ----

// EXT-VECTOR k-NN (k=10) — requires a vector index on :Post(embedding)
CALL db.index.vector.queryNodes('post_embedding', 10, $queryEmbedding)
YIELD node, score RETURN node, score;

// EXT-VECTOR hybrid (traverse forum->posts then rank): Neo4j cannot fuse graph
// traversal and ANN ranking into one planned operation — see README footnote 5.
// This is therefore NOT provided as a single-query workload.
