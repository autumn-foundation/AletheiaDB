import re

with open('tests/sentry_temporal.rs', 'r') as f:
    content = f.read()

# Since we appended tests earlier by accident and didn't remove them.
# I will find the first line number of `fn test_timerange_contains_mutants_strict` and keep the first one
# Actually it's easier to simply take the substring before the duplicated section since it was appended at the end

# `test_timerange_contains_mutants_strict` appears twice. We'll cut off everything after the first EOF was appended.
# Or just find the string `#[test]\nfn test_timerange_contains_mutants_strict()`
index = content.rfind('#[test]\nfn test_timerange_contains_mutants_strict()')
if index != -1:
    content = content[:index]

with open('tests/sentry_temporal.rs', 'w') as f:
    f.write(content)
