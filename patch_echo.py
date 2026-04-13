import sys

with open("src/experimental/echo.rs", "r") as f:
    content = f.read()

to_remove = """pub trait Resonator {
    /// Generate a fingerprint from the given history.
    fn resonate(&self, history: &EntityHistory) -> TemporalFingerprint;
}

"""
content = content.replace(to_remove, "")
content = content.replace("impl Resonator for ActivityDensityResonator {", "impl ActivityDensityResonator {")
content = content.replace("    fn resonate(&self", "    /// Generate a fingerprint from the given history.\n    pub fn resonate(&self")
content = content.replace("resonator: Box<dyn Resonator>,", "resonator: ActivityDensityResonator,")
content = content.replace("resonator: Box::new(ActivityDensityResonator::default()),", "resonator: ActivityDensityResonator::default(),")
content = content.replace("pub fn with_resonator<R: Resonator + 'static>(mut self, resonator: R) -> Self {", "pub fn with_resonator(mut self, resonator: ActivityDensityResonator) -> Self {")
content = content.replace("self.resonator = Box::new(resonator);", "self.resonator = resonator;")
content = content.replace("pub fn with_resonator<R: Resonator + 'static>(self, resonator: R) -> Self {", "pub fn with_resonator(self, resonator: ActivityDensityResonator) -> Self {")

with open("src/experimental/echo.rs", "w") as f:
    f.write(content)
