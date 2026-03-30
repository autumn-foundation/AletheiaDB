import os
import glob

def replace_in_file(filepath, old, new):
    with open(filepath, "r") as f:
        content = f.read()
    if old in content:
        with open(filepath, "w") as f:
            f.write(content.replace(old, new))

for f in glob.glob("src/**/*.rs", recursive=True):
    replace_in_file(f, "builder.limit(limit + offset).execute(&self.db)", "builder.limit(limit + offset).execute(self.db.as_ref())")
    replace_in_file(f, "builder.execute(&self.db)", "builder.execute(self.db.as_ref())")
    replace_in_file(f, "builder.limit(limit).execute(&self.db)", "builder.limit(limit).execute(self.db.as_ref())")
    replace_in_file(f, ".execute(&db)", ".execute(db.as_ref())")

# Wait, `.execute(&db)` in `http/handlers.rs` where `db` is `Arc<AletheiaDB>`. Let's fix that.
# Let's just fix it generally.
