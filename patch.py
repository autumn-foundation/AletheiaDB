with open(".jules/bolt.md", "r") as f:
    content = f.read()

content = content.replace("<<<<<<< HEAD\n=======\n", "")
content = content.replace(">>>>>>> jules-test\n", "")

with open(".jules/bolt.md", "w") as f:
    f.write(content)
