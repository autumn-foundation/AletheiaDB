import os
import re

adr_dir = 'docs/adr'
files = [f for f in os.listdir(adr_dir) if f.endswith('.md') and re.match(r'^\d{4}-.*\.md$', f)]
files.sort()

adrs = []
for f in files:
    with open(os.path.join(adr_dir, f), 'r', encoding='utf-8') as file:
        content = file.read()

        # Extract ID and Title
        title_match = re.search(r'^#\s*(ADR-)?(\d{4})[.:]?\s*(.*)$', content, re.MULTILINE)
        if title_match:
            num = title_match.group(2)
            title = title_match.group(3).strip()
            # fallback if title extraction missed it
            if not title:
                title_match = re.search(r'^#\s*(.*)$', content, re.MULTILINE)
                title = title_match.group(1).strip()
        else:
            num = f[:4]
            title_match = re.search(r'^#\s*(.*)$', content, re.MULTILINE)
            title = title_match.group(1).strip() if title_match else f[5:-3].replace('-', ' ').title()

        # Clean title
        if title.startswith(f"ADR-{num}: "):
            title = title[len(f"ADR-{num}: "):]
        elif title.startswith(f"{num}. "):
            title = title[len(f"{num}. "):]

        # Extract Status
        status_match = re.search(r'\*\*Status:\*\*\s*(.*)$', content, re.MULTILINE)
        if not status_match:
            status_match = re.search(r'^Status:\s*(.*)$', content, re.MULTILINE)
        status = status_match.group(1).strip() if status_match else "Unknown"
        # Simplify status to first word mostly (e.g. "Accepted (Implemented)" -> "Accepted")
        status_clean = status.split(' ')[0]
        if status_clean.lower() == 'superseded':
             status_clean = 'Superseded'

        # Extract Date
        date_match = re.search(r'\*\*Date:\*\*\s*(.*)$', content, re.MULTILINE)
        if not date_match:
            date_match = re.search(r'^Date:\s*(.*)$', content, re.MULTILINE)
        date = date_match.group(1).strip() if date_match else "Unknown"
        # Simplify Date to first token
        date = date.split(' ')[0]

        # Extract Categories
        cat_match = re.search(r'\*\*Categories:\*\*\s*(.*)$', content, re.MULTILINE)
        if not cat_match:
            cat_match = re.search(r'^Categories:\s*(.*)$', content, re.MULTILINE)
        categories = cat_match.group(1).strip() if cat_match else "none"

        if num == "0000":
            continue

        adrs.append({
            'id': f"ADR-{num}",
            'filename': f,
            'title': title,
            'status': status_clean,
            'date': date,
            'categories': categories
        })

accepted = [a for a in adrs if a['status'].lower() == 'accepted']
proposed = [a for a in adrs if a['status'].lower() == 'proposed']
other = [a for a in adrs if a['status'].lower() not in ['accepted', 'proposed']]

def generate_table(items):
    lines = []
    lines.append("| ID | Title | Date | Categories |")
    lines.append("|----|-------|------|------------|")
    for a in items:
        lines.append(f"| [{a['id']}]({a['filename']}) | {a['title']} | {a['date']} | {a['categories']} |")
    return "\n".join(lines)

with open(os.path.join(adr_dir, 'README.md'), 'r', encoding='utf-8') as f:
    readme = f.read()

index_start = readme.find('## ADR Index')
if index_start == -1:
    print("Could not find '## ADR Index' in README.md")
    exit(1)

# Find where the next major section starts (or end of file)
creating_start = readme.find('## Creating a New ADR', index_start)

new_index = "## ADR Index\n\n### Accepted\n\n"
new_index += generate_table(accepted) + "\n\n"

if proposed:
    new_index += "### Proposed\n\n"
    new_index += generate_table(proposed) + "\n\n"

if other:
    new_index += "### Other (Deprecated/Superseded)\n\n"
    new_index += generate_table(other) + "\n\n"

new_readme = readme[:index_start] + new_index + readme[creating_start:]

with open(os.path.join(adr_dir, 'README.md'), 'w', encoding='utf-8') as f:
    f.write(new_readme)

print("ADR Index generated successfully.")
