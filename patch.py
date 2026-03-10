with open("src/experimental/temporal_narrative.rs", "r") as f:
    content = f.read()

content = content.replace(
"""        // Construct a fake NarrativeGenerator to test method panic
        // Safety: NarrativeGenerator is a ZST with PhantomData, so it's valid to transmute from unit or zeroed.
        // We just need it to call the method.
        let generator: NarrativeGenerator<'_> = unsafe { std::mem::transmute(()) };""",
"""        // Construct a fake NarrativeGenerator to test method panic
        let generator: NarrativeGenerator<'_> = NarrativeGenerator {
            _marker: std::marker::PhantomData,
        };"""
)

with open("src/experimental/temporal_narrative.rs", "w") as f:
    f.write(content)
