**Remove Unused Payload in Enum Variants**
**Learning:** Found that string allocations on `Token::Other(String)` for unrecognized tokens were completely unused, creating massive O(N) heap allocations for every word parsed in the SQL queries.
**Action:** When creating enum variants for parser tokens that are only used as "catch-alls", prefer unit variants (`Token::Other`) over variants that capture unused text payloads (`Token::Other(String)`).
