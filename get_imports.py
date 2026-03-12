import os
import re

for root, _, files in os.walk('src'):
    for file in files:
        if file.endswith('.rs'):
            filepath = os.path.join(root, file)
            with open(filepath, 'r') as f:
                content = f.read()
                imports = re.findall(r'use\s+([^;]+);', content)
                for imp in imports:
                    if 'index::temporal' in imp:
                        print(f"{filepath}: {imp}")
