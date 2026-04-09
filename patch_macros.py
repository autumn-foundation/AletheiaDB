import re

with open('src/core/macros.rs', 'r') as f:
    content = f.read()

# Replace the macro definition to avoid ambiguity
new_macro = '''#[macro_export]
macro_rules! match_ignore_ascii_case {
    (
        $target:expr,
        $( $pattern:expr => $result:expr ),+
        , _ => $default:expr
        $(,)?
    ) => {
        {
            let target = $target;
            $(
                if target.eq_ignore_ascii_case($pattern) {
                    $result
                } else
            )+
            {
                $default
            }
        }
    };
    (
        $target:expr,
        $( $pattern:expr => $result:expr ),+
        $(,)?
    ) => {
        {
            let target = $target;
            $(
                if target.eq_ignore_ascii_case($pattern) {
                    $result
                } else
            )+
            {
                // This will fail at runtime if no default is provided and we reach here
                panic!("Unhandled case in match_ignore_ascii_case!");
            }
        }
    };
}'''

content = re.sub(r'#\[macro_export\].*?\}', new_macro, content, flags=re.DOTALL | re.MULTILINE)

with open('src/core/macros.rs', 'w') as f:
    f.write(content)
