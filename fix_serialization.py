with open("src/core/vector/serialization.rs", "r") as f:
    c = f.read()

c = c.replace('''pub(crate) fn validate_vector_dimensions(len: usize) -> Result<()> {
    if len > MAX_VECTOR_DIMENSIONS {
        return Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension: len,
            max_allowed: MAX_VECTOR_DIMENSIONS,
        }));
    }
    Ok(())
}''', '''pub(crate) fn validate_vector_dimensions(len: usize) -> Result<()> {
    if len > MAX_VECTOR_DIMENSIONS {
        return Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension: len,
            max_allowed: MAX_VECTOR_DIMENSIONS,
        }));
    }
    Ok(())
}

pub(crate) fn validate_vector_values(v: &[f32]) -> Result<()> {
    let mut nan_count = 0;
    let mut inf_count = 0;

    for &val in v {
        if val.is_nan() {
            nan_count += 1;
        } else if val.is_infinite() {
            inf_count += 1;
        }
    }

    if nan_count > 0 {
        return Err(Error::Vector(VectorError::ContainsNaN { count: nan_count }));
    }
    if inf_count > 0 {
        return Err(Error::Vector(VectorError::ContainsInfinity { count: inf_count }));
    }
    Ok(())
}''')

c = c.replace('''pub fn try_serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) -> Result<()> {
    // Defensive check: vectors should be validated at construction via PropertyValue::vector()
    validate_vector_dimensions(v.len())?;''', '''pub fn try_serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) -> Result<()> {
    // Defensive check: vectors should be validated at construction via PropertyValue::vector()
    validate_vector_dimensions(v.len())?;
    validate_vector_values(v)?;''')

c = c.replace('''        // Big-endian fallback: convert each element individually
        let mut values = Vec::with_capacity(dimension);
        for chunk in data_slice.chunks_exact(4) {
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

    Ok((Arc::from(values.into_boxed_slice()), total_len))''', '''        // Big-endian fallback: convert each element individually
        let mut values = Vec::with_capacity(dimension);
        for chunk in data_slice.chunks_exact(4) {
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

    validate_vector_values(&values)?;

    Ok((Arc::from(values.into_boxed_slice()), total_len))''')

c = c.replace('''    #[cfg(not(target_endian = "little"))]
    let values = {
        let mut values = Vec::with_capacity(nnz);
        for chunk in values_slice.chunks_exact(4) {
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

    // Construct SparseVec (this will validate the data)''', '''    #[cfg(not(target_endian = "little"))]
    let values = {
        let mut values = Vec::with_capacity(nnz);
        for chunk in values_slice.chunks_exact(4) {
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

    validate_vector_values(&values)?;

    // Construct SparseVec (this will validate the data)''')

with open("src/core/vector/serialization.rs", "w") as f:
    f.write(c)
