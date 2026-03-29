import re

with open("temporal_mutants.txt", "r") as f:
    lines = f.readlines()

for line in lines:
    print(line.strip())
