**[Unwrap in parsing and deserialization]**
**Learning:** `unwrap()` was used frequently in parsing routines (like `lexer.rs` for `advance()`) and when converting safely slice sizes using `try_into()` (in `hlc.rs`, `hasher.rs`, `value.rs`, `map.rs`). In testing or isolated functions these are safe but could crash if assumptions slightly shift.
**Action:** Replace `unwrap()` with `expect("descriptive message")` especially where fixed sized slices are known to be correct, and fix tests where parsing unwrap masked actual error conditions (like unterminated strings or invalid input boundaries).
