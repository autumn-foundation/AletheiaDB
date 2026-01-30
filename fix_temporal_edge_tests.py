#!/usr/bin/env python3
import re

with open('tests/temporal_edge_tests.rs', 'r') as f:
    content = f.read()

# Pattern 1: Replace (timestamp, timestamp) with (timestamp, tx_time_for_query)
# First, we need to find where timestamps are captured and add tx_time capture

# Replace the timestamp capture pattern to also capture tx_time
old_capture = r'\(version\.temporal\.valid_time\(\)\.start\(\)\.wallclock\(\) \+ 1\)\.into\(\)'
new_capture = r'{
            let version = hist_guard.get_edge_version(version_id).unwrap();
            let valid_time = (version.temporal.valid_time().start().wallclock() + 1).into();
            let tx_time = (version.temporal.transaction_time().start().wallclock() + 1000).into();
            (valid_time, tx_time)
        }'

# This is tricky - let's do a more targeted approach
# Replace .find_edge_version_at_time(edge_id, timestamp, timestamp)
# with .find_edge_version_at_time(edge_id, timestamp.0, timestamp.1)

# Actually, let's just fix the specific calls we know about
# Lines around 126, 239, 349, 366, 615

print("Analyzing file...")
lines = content.split('\n')

# Track which lines have find_edge_version_at_time calls
for i, line in enumerate(lines):
    if 'find_edge_version_at_time' in line and ', t1, t1)' in line:
        print(f"Line {i+1}: Found ', t1, t1)' pattern")
    elif 'find_edge_version_at_time' in line and ', t2, t2)' in line:
        print(f"Line {i+1}: Found ', t2, t2)' pattern")
    elif 'find_edge_version_at_time' in line and ', timestamp, timestamp)' in line:
        print(f"Line {i+1}: Found ', timestamp, timestamp)' pattern")

print("\nThis file needs manual fixes - the timestamp variables need to be tuples.")
