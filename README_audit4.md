Okay, I'm finding friction points.

Friction 1: No `From<u64>` or `TryFrom<u64>` for `NodeId` makes it cumbersome to construct.
Friction 2: No way to create a vector from an array `&[0.1f64; 10]` because `PropertyValue` doesn't implement `From<&[f64]>`. So `vec![0.1f64; 10]` in properties! fails with confusing error.

Wait, are there any other confusing things?
What if I try to use `as_of` in a hybrid query builder without giving a valid valid_time and tx_time type?
