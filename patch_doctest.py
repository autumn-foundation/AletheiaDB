import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# Fix the doctest
doctest_pattern = r"""// \[dependencies\]
// aletheiadb = \{ version = "0\.1", features = \["nova"\] \}

# #\[cfg\(feature = "nova"\)\]
# \{
use aletheiadb::AletheiaDB;"""

new_doctest = """// [dependencies]
// aletheiadb = { version = "0.1", features = ["nova"] }

# #[cfg(feature = "nova")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use aletheiadb::AletheiaDB;"""

content = content.replace(doctest_pattern, new_doctest)

main_pattern = r"""# fn main\(\) -> Result<\(\), Box<dyn std::error::Error>> \{
let db = AletheiaDB::new\(\)\?;"""

new_main = """let db = AletheiaDB::new()?;"""

content = re.sub(main_pattern, new_main, content, flags=re.MULTILINE)

end_pattern = r"""# Ok\(\)\n# \}\n# \}\n# #\[cfg\(not\(feature = "nova"\)\)\]\n# fn main\(\) \{\}"""

new_end = """# Ok()\n# }\n# #[cfg(not(feature = "nova"))]\n# fn main() {}"""

content = content.replace(end_pattern, new_end)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
