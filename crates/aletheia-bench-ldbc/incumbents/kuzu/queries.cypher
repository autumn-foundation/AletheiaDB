// KuzuDB Cypher mappings for the same logical queries (NOT executed here).
// KuzuDB uses a typed schema; assume node/rel tables Person, KNOWS, Forum,
// CONTAINER_OF, Post, HAS_CREATOR, HAS_TAG, Tag, Comment, REPLY_OF created by
// kuzu/run.sh's DDL from the shared generated data.
//
// Temporal-extension queries are ABSENT: KuzuDB has no native bi-temporal
// reconstruction (README footnote 1).

// IS1 — person profile
MATCH (p:Person) WHERE p.id = $personId RETURN p;

// IS2 — messages authored by a person
MATCH (p:Person)<-[:HAS_CREATOR]-(m) WHERE p.id = $personId RETURN m;

// IS3 — direct friends
MATCH (p:Person)-[:KNOWS]->(f:Person) WHERE p.id = $personId RETURN f;

// IS5 — creator of a post
MATCH (post:Post)-[:HAS_CREATOR]->(c:Person) WHERE post.id = $postId RETURN c;

// IS6 — forum of a post
MATCH (post:Post)<-[:CONTAINER_OF]-(forum:Forum) WHERE post.id = $postId RETURN forum;

// IC1 — friends within 2 hops
MATCH (p:Person)-[:KNOWS*1..2]->(f:Person) WHERE p.id = $personId AND f.id <> $personId
RETURN DISTINCT f;

// IC2 — messages by direct friends
MATCH (p:Person)-[:KNOWS]->(f:Person)<-[:HAS_CREATOR]-(m) WHERE p.id = $personId RETURN m;

// IC9 — messages by friends-of-friends
MATCH (p:Person)-[:KNOWS*1..2]->(f:Person)<-[:HAS_CREATOR]-(m)
WHERE p.id = $personId AND f.id <> $personId RETURN m;

// INS1 — insert person
CREATE (:Person {id: $newId, name: $name, city: $city});

// INS6 — insert post + creator edge (two statements in KuzuDB)
CREATE (:Post {id: $newId, length: 100});
MATCH (post:Post), (c:Person) WHERE post.id = $newId AND c.id = $creatorId
CREATE (post)-[:HAS_CREATOR]->(c);

// UPD — update person city
MATCH (p:Person) WHERE p.id = $personId SET p.city = $city;

// EXT-VECTOR k-NN (KuzuDB vector extension; expressible, NOT measured here)
// CALL QUERY_VECTOR_INDEX('Post', 'post_embedding', $queryEmbedding, 10)
//   YIELD node, distance RETURN node, distance;

// EXT-VECTOR hybrid (single-query graph+vector): NOT expressible as one
// operation (README footnote 5).
