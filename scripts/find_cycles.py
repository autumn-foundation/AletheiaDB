import os
import re
from collections import defaultdict

def get_imports(file_path):
    imports = set()
    with open(file_path, 'r', encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if line.startswith('use crate::'):
                parts = line.split('::')
                if len(parts) > 1:
                    module = parts[1].split(';')[0].split('{')[0].strip()
                    imports.add(module)
            elif line.startswith('use super::'):
                # This is harder to resolve without full context, skipping for now
                pass
    return imports

def get_top_level_modules(root_dir):
    modules = set()
    try:
        for name in os.listdir(root_dir):
            path = os.path.join(root_dir, name)
            if os.path.isdir(path):
                if name not in ['bin', 'target', '.git']:
                    modules.add(name)
            elif name.endswith('.rs'):
                module_name = name[:-3]
                if module_name not in ['lib', 'main']:
                    modules.add(module_name)
    except OSError:
        pass
    return modules

def scan_directory(root_dir):
    dependencies = defaultdict(set)
    top_level_modules = get_top_level_modules(root_dir)

    for dirpath, _, filenames in os.walk(root_dir):
        if 'target' in dirpath or '.git' in dirpath:
            continue

        current_module = None
        rel_path = os.path.relpath(dirpath, root_dir)

        # Determine which top-level module this file belongs to
        if rel_path == '.':
            # Files in src root
            for filename in filenames:
                if filename.endswith('.rs'):
                    module_name = filename[:-3]
                    if module_name in top_level_modules:
                        current_module = module_name
                        file_path = os.path.join(dirpath, filename)
                        file_imports = get_imports(file_path)
                        for imp in file_imports:
                            if imp != current_module and imp in top_level_modules:
                                dependencies[current_module].add(imp)
            continue

        parts = rel_path.split(os.sep)
        if len(parts) > 0:
            current_module = parts[0]

        if not current_module or current_module not in top_level_modules:
            continue

        for filename in filenames:
            if filename.endswith('.rs'):
                file_path = os.path.join(dirpath, filename)
                file_imports = get_imports(file_path)

                for imp in file_imports:
                    if imp != current_module and imp in top_level_modules:
                        dependencies[current_module].add(imp)

    return dependencies

def find_cycles(graph):
    visited = set()
    path = []
    cycles = []

    def visit(node):
        if node in path:
            cycle = path[path.index(node):] + [node]
            cycles.append(cycle)
            return
        if node in visited:
            return

        visited.add(node)
        path.append(node)

        for neighbor in graph[node]:
            visit(neighbor)

        path.pop()

    for node in list(graph.keys()):
        visit(node)

    return cycles

if __name__ == "__main__":
    deps = scan_directory("src")
    print("Dependencies:")
    for mod, imports in deps.items():
        print(f"{mod} -> {', '.join(sorted(imports))}")

    print("\nCycles:")
    cycles = find_cycles(deps)
    if cycles:
        for cycle in cycles:
            print(" -> ".join(cycle))
    else:
        print("No cycles found among top-level modules.")
