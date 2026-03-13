with open("src/core/property.rs", "r") as f:
    c = f.read()

c = c.replace('''    pub fn try_vector<V: AsRef<[f32]>>(v: V) -> Result<Self> {
        let slice = v.as_ref();
        validate_vector_dimensions(slice.len())?;
        Ok(PropertyValue::Vector(Arc::from(slice)))
    }''', '''    pub fn try_vector<V: AsRef<[f32]>>(v: V) -> Result<Self> {
        let slice = v.as_ref();
        validate_vector_dimensions(slice.len())?;
        crate::core::vector::serialization::validate_vector_values(slice)?;
        Ok(PropertyValue::Vector(Arc::from(slice)))
    }''')

with open("src/core/property.rs", "w") as f:
    f.write(c)
