**Remove String payload from unused enum variants**
**Learning:** In parsers, creating heap-allocated `String`s for unrecognized or ignored tokens introduces a massive allocation overhead on the hot path.
**Action:** Replace `EnumVariant(String)` with `EnumVariant` if the parsed content is never consumed, avoiding `$N` allocations where `$N` is the number of ignored words.
