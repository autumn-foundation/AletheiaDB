import subprocess
import json

p = subprocess.run(["cargo", "llvm-cov", "report", "--json", "--ignore-filename-regex", "test|bench", "--workspace"], capture_output=True, text=True)
if p.returncode != 0:
    print("Failed to run llvm-cov")
else:
    data = json.loads(p.stdout)
    for file in data.get('files', []):
        name = file['filename']
        lines = file['summary']['lines']
        if lines['count'] > 0 and lines['percent'] < 90:
            print(f"{name}: {lines['percent']}%")
