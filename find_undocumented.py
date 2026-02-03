
import os
import re

def count_lines(filepath):
    with open(filepath, 'r') as f:
        lines = f.readlines()

    code_lines = 0
    doc_lines = 0

    in_block_comment = False

    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue

        if in_block_comment:
            if '*/' in stripped:
                in_block_comment = False
            doc_lines += 1
            continue

        if stripped.startswith('/*'):
            doc_lines += 1
            if '*/' not in stripped:
                in_block_comment = True
            continue

        if stripped.startswith('///') or stripped.startswith('//!'):
            doc_lines += 1
        elif stripped.startswith('//'):
            pass # Regular comment
        else:
            code_lines += 1

    return code_lines, doc_lines

def main():
    results = []
    for root, dirs, files in os.walk('src'):
        for file in files:
            if file.endswith('.rs'):
                filepath = os.path.join(root, file)
                code, doc = count_lines(filepath)
                if code > 0:
                    ratio = doc / code
                    results.append((filepath, ratio, code, doc))

    results.sort(key=lambda x: x[1])

    print(f"{'File':<60} {'Ratio':<10} {'Code':<10} {'Doc':<10}")
    print("-" * 90)
    for filepath, ratio, code, doc in results:
        if ratio < 0.2 and code > 20: # Filter out tiny files or files with very little code
             print(f"{filepath:<60} {ratio:.2f}       {code:<10} {doc:<10}")

if __name__ == "__main__":
    main()
