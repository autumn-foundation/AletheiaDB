Let's review the code execution again.
In the basic graph example:
`db.create_edge(alice_id, bob_id, "KNOWS", properties! {})?;`
Wait! `properties! {}` expects either an explicit type or a default. Does it compile? Yes, it compiled.

What about "The "Error Check": Trigger errors on purpose. Are the messages helpful?"
Let's write a small script to trigger an error, like getting a node that doesn't exist.
