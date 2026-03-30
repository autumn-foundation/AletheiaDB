import re
from pathlib import Path

content = Path('src/honeycomb/event.rs').read_text()

# Less brittle removal
trait_pattern = r'/// Trait for types that can hold fields \(compatible with libhoney\'s FieldHolder\)\.[\s\S]*?impl FieldHolder for Event \{[\s\S]*?\}\n'

new_content = re.sub(trait_pattern, '', content)

if new_content != content:
    print("Successfully matched and removed FieldHolder")
    Path('src/honeycomb/event.rs').write_text(new_content)
else:
    print("Failed to match FieldHolder")
