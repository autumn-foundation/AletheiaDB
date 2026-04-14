# 🗣️ Echo: Getting Started example is broken

## 🤦 The Confusion:
When I copy-pasted the Query Language (AQL) example from the `README.md` and ran it, I immediately got a runtime error:
`Error: Query(InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch." })`.

The README shows an example using an ISO string (`'2024-01-15T10:00:00Z'`), but the system crashed complaining it needed microseconds!

## 🕵️ The Reality:
Turns out, the `execute_aql` method actually expects temporal `AS OF` query parameters to be formatted as integer timestamps in microseconds since the Unix epoch (e.g., `1705312800000000`), rather than ISO 8601 formatted strings. The documentation completely contradicted the code requirements.

## 💡 The Fix:
I updated the AQL examples in `README.md` to use the correct integer microsecond timestamps instead of ISO 8601 strings so that the example code actually works when copied.
