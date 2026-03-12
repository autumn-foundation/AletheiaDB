import subprocess
import time

def run_tests_with_timeout(cmd, timeout=30):
    try:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=True)
        stdout, stderr = proc.communicate(timeout=timeout)
        return proc.returncode, stdout.decode(), stderr.decode()
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()
        return -1, "Timeout", ""

def get_surviving_mutants(file_path):
    print(f"Running cargo mutants on {file_path}")

    cmd = f"cargo mutants --list --file {file_path}"
    ret, stdout, stderr = run_tests_with_timeout(cmd, timeout=30)

    if ret != 0:
        print(f"Error listing mutants: {stderr}")
        return []

    mutants = stdout.strip().split('\n')
    return [m for m in mutants if m]

mutants = get_surviving_mutants("src/query/planner/rules/limit_pushdown.rs")
print(f"Found {len(mutants)} mutants")
for i, m in enumerate(mutants):
    print(f"{i}: {m}")
