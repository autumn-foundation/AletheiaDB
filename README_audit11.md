It's NOT in the prelude!
So "Query Language" section in README says:
```cypher
-- Basic graph query
MATCH (n:Person {name: "Alice"})-[:KNOWS]->(friend:Person)
RETURN friend
```
But it DOES NOT show the Rust code to run this query!
So if I am a user trying to use the Query Language, I copy the SQL/Cypher snippet, but where do I paste it?
I have to guess `db.query_something("MATCH ...")`. But AletheiaDB doesn't have such a method! I have to manually use `parse_query("...").unwrap()` and pass it to `db.execute_query(query)`.
This is exactly the type of friction Echo is supposed to find!

Let's do the walkthrough.
**Scenario:** "I am a new user trying to use the Query Language from the README to run `MATCH (n:Person) RETURN n`."
**Action:** Try to use the API based *only* on the public docs/examples.
The README provides a Cypher query but no Rust example showing how to execute it!
**Stumble:** "I see the AQL syntax, but what Rust method do I call? `db.query("MATCH...")`? Nope, that returns a `QueryBuilder`. `db.execute("MATCH...")`? Doesn't exist. I have to dig into the source code to find `parse_query` in the `query` module!"
**Report:**
Create an Issue (or PR with a 'Docs Fix' request):
- Title: "🗣️ Echo: Query Language (AQL) example is missing Rust execution code"
- Description:
* 🤦 **The Confusion:** "Tried to run the Cypher queries from the 'Query Language (AQL)' section in the README. But there is no Rust code showing *how* to execute them! Tried `db.query("...")` but it expects no arguments and returns a builder."
* 🕵️ **The Reality:** "Turns out I have to import `aletheiadb::query::parse_query` (which isn't in the prelude), parse the string, handle the error, and then pass it to `db.execute_query()`. Or better yet, AletheiaDB should just have a `db.execute_aql(&str)` method."
* 💡 **The Fix:** "Add a convenient `execute_aql` method directly to `AletheiaDB` and update the README to show the actual Rust code to run the queries, plus export `parse_query` in the prelude."

Wait, what if `db.execute_aql` already exists? Let me check `src/db/mod.rs` or `src/api/` for `execute_aql` or similar.
