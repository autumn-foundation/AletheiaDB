import subprocess
import re

def get_changed_files():
    result = subprocess.run(["git", "show", "--name-only", "--format="], capture_output=True, text=True)
    files = result.stdout.strip().split("\n")
    return [f for f in files if f.endswith(".rs")]

def review_files(files):
    for file in files:
        if not file.strip():
             continue
        print(f"Reviewing {file}")
        # Add your rust core review logic here

if __name__ == "__main__":
    files = get_changed_files()
    review_files(files)
