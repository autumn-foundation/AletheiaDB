with open("tests/hybrid_query.rs", "r") as f:
    content = f.read()

content = content.replace("traverse_and_rank(&*db_clone,", "traverse_and_rank(&db_clone,")
content = content.replace("traverse_and_rank(&*db,", "traverse_and_rank(&db,")

with open("tests/hybrid_query.rs", "w") as f:
    f.write(content)
