import re

with open('src/core/temporal.rs', 'r') as f:
    content = f.read()

def replace_fn_body(name, new_body):
    pattern = re.compile(r'(fn ' + name + r'\(\) \{).*?(^\s*\})\n', re.MULTILINE | re.DOTALL)
    for match in pattern.finditer(content):
        start_idx = match.start(1)
        brace_count = 0
        end_idx = -1
        for i in range(start_idx + len(match.group(1)), len(content)):
            if content[i] == '{':
                brace_count += 1
            elif content[i] == '}':
                if brace_count == 0:
                    end_idx = i
                    break
                else:
                    brace_count -= 1
        if end_idx != -1:
            original = content[start_idx:end_idx+1]
            replacement = match.group(1) + '\n' + new_body + '\n    }'
            return content.replace(original, replacement)
    return content

# The reviewer said the previous solution was broken. Wait, the memory instruction exactly said:
# "In AletheiaDB, when testing `time::to_iso8601` to kill arithmetic mutants, do not dynamically reconstruct the `SystemTime` debug output (e.g., using `format!("{:?}", std::time::UNIX_EPOCH...)`), as this violates test independence ('The Mirror Test' tautology). Instead, use independently derived, hardcoded string values for expected assertions, even if it requires platform-specific `#[cfg(windows)]` branches."
# This is exactly what I did, but Codecov is complaining because the `#[cfg(windows)]` branches are not executed on Linux, lowering coverage below 85%!
# Ah. If I use `#[cfg(windows)]` and `#[cfg(not(windows))]` on the code itself, Codecov considers the uncompiled code on the other OS as "not covered by tests".
# Wait. If I use `if cfg!(windows)` instead of `#[cfg(windows)]`, the branch is always compiled, but one side is dead code at runtime, which Codecov might flag.
# If I use `if cfg!(windows)`, Codecov says "Line was not covered by tests" for the side that isn't executed.
# Wait, how to fix codecov coverage issue when using `if cfg!(windows)`?
# I can just use `std::time::UNIX_EPOCH` to format it exactly like the function does! Wait, memory explicitly said "do not dynamically reconstruct... use independently derived, hardcoded string values".
# But Codecov complains! How do I fix Codecov while keeping the hardcoded strings and avoiding `if cfg!(windows)` runtime uncovered branches?
# What if I construct the test using `#[cfg(windows)]` on the entire test function?
# Let's try that.
