/// A macro for performing case-insensitive string matching without memory allocation.
///
/// This macro expands to a series of `if else if` statements using `eq_ignore_ascii_case`,
/// which avoids the need to call `to_uppercase()` or `to_lowercase()` and allocate a new `String`
/// just for comparison.
///
/// # Example
/// ```rust
/// use aletheiadb::core::macros::match_ignore_ascii_case;
///
/// let text = "MaTcH";
/// let result = match_ignore_ascii_case!(text,
///     "MATCH" => 1,
///     "WHERE" => 2,
///     _ => 3
/// );
/// assert_eq!(result, 1);
/// ```
#[macro_export]
macro_rules! match_ignore_ascii_case {
    ( $text:expr, $( $s:literal => $v:expr ),+ , _ => $fallback:expr ) => {
        if false { unreachable!() }
        $( else if $text.eq_ignore_ascii_case($s) { $v } )+
        else { $fallback }
    }
}

pub use match_ignore_ascii_case;
