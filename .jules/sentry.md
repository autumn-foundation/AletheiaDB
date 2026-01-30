## 2024-05-23 - [Recursion Limit in Parser]
**Learning:** Recursive descent parsers without depth limits are vulnerable to stack overflow attacks (DoS) via deeply nested structures (like parentheses or chained operators).
**Action:** Always implement a recursion guard (e.g., `depth` counter) in recursive parsing functions and define a safe maximum depth (e.g., 100).
