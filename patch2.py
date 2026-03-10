import re

with open("src/experimental/temporal_narrative.rs", "r") as f:
    content = f.read()

# Replace transmute implementation with PhantomData, being careful about exact spacing
# The issue explicitly says "Replaced `unsafe { std::mem::transmute(()) }` with `NarrativeGenerator { _marker: std::marker::PhantomData }`"

content = re.sub(
    r"unsafe\s*\{\s*std::mem::transmute\(\(\)\)\s*\}",
    r"NarrativeGenerator { _marker: std::marker::PhantomData }",
    content
)

with open("src/experimental/temporal_narrative.rs", "w") as f:
    f.write(content)
