#!/usr/bin/env python3
import re

with open('tests/backend_parity.rs', 'r') as f:
    content = f.read()

# Fix IngestOptions::new calls
# Pattern: &IngestOptions::new(\n    ...\n    0.25,\n    ),
pattern1 = r'(&IngestOptions::new\(\s+Some\([^)]+\),\s+&[^,]+,\s+(?:None|Some\(&[^)]+\)),\s+ChunkingStrategy::Fixed,\s+0\.25,\s+\),)'

def fix_ingest_options(match):
    text = match.group(1)
    # Add 20, and true, before the closing )
    return text.replace('0.25,', '0.25,\n                20,\n                true,')

content = re.sub(pattern1, fix_ingest_options, content, flags=re.DOTALL)

# Fix let options = IngestOptions::new(
pattern2 = r'(let options = IngestOptions::new\(\s+Some\(&harness\.project\),\s+&\[\],\s+None,\s+ChunkingStrategy::Fixed,\s+0\.25,\s+\);)'

def fix_options(match):
    text = match.group(1)
    return text.replace('0.25,', '0.25,\n        20,\n        true,')

content = re.sub(pattern2, fix_options, content, flags=re.DOTALL)

# Fix pipeline::ingest_path calls
pattern3 = r'(pipeline::ingest_path\(\s+&harness\.storage,\s+&embedder,\s+None,\s+&article_url,\s+Some\(&harness\.project\),\s+&no_tags,\s+ChunkingStrategy::Fixed,\s+0\.25,\s+\))'

def fix_ingest_path(match):
    text = match.group(1)
    return text.replace('0.25,', '0.25,\n            20,\n            true,')

content = re.sub(pattern3, fix_ingest_path, content, flags=re.DOTALL)

with open('tests/backend_parity.rs', 'w') as f:
    f.write(content)

print("Fixed tests/backend_parity.rs")
