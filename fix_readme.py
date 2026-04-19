with open("README.md", "r") as f:
    content = f.read()

# Fix the execute() line
old_line = "    .execute()?\n"
new_line = "    .execute(&db)?\n"
if ".execute()?" in content:
    content = content.replace(old_line, new_line)
    print("Found .execute()?")

old_line_2 = "    .execute(&db)?\n"
if ".execute(&db)?" in content:
    print("Wait, it was already .execute(&db)?")

with open("README.md", "w") as f:
    f.write(content)
