import os

def check_file(filepath):
    with open(filepath, 'r') as f:
        content = f.read()

    lines = content.split('\n')
    missing_docs = []

    for i, line in enumerate(lines):
        line = line.strip()
        if line.startswith('pub struct ') or line.startswith('pub enum ') or line.startswith('pub fn ') or line.startswith('pub trait '):
            # Check if previous lines have ///
            has_doc = False
            for j in range(i-1, max(-1, i-15), -1):
                prev_line = lines[j].strip()
                if prev_line.startswith('///'):
                    has_doc = True
                    break
                elif not prev_line.startswith('#') and not prev_line == '' and not prev_line.startswith('//'):
                    break

            if not has_doc:
                missing_docs.append(f"Line {i+1}: {line}")

    return missing_docs

for root, _, files in os.walk('src'):
    for file in files:
        if file.endswith('.rs'):
            filepath = os.path.join(root, file)
            missing = check_file(filepath)
            if missing:
                print(f"Missing docs in {filepath}:")
                for m in missing:
                    print(f"  {m}")
