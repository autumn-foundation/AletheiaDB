## [Array Pre-allocation Buffer Overflow]
**Learning:** Found a potential Denial of Service vulnerability in the binary deserialization path for `PropertyValue::Array`. An attacker could specify a massive element count within the `MAX_ARRAY_ELEMENTS` limit but provide a truncated buffer. Without a check verifying the minimum required bytes per element, this could trigger a large pre-allocation.
**Action:** Always write a targeted test covering extreme valid counts but insufficient data buffers for length-prefixed protocol parsing (e.g. Arrays, Strings, Vector dimensions).
