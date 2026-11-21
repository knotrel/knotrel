#!/usr/bin/env python3
"""Prepare existing Markdown for Jekyll without changing the source docs."""
import pathlib
import re
import shutil
import sys
from urllib.parse import urlsplit, urlunsplit

root = pathlib.Path(__file__).resolve().parents[1]
source = root / 'docs'
dest = pathlib.Path(sys.argv[1]).resolve()
if dest == source or source in dest.parents:
    raise SystemExit('destination must be outside docs')
shutil.copytree(source, dest, dirs_exist_ok=True)
pattern = re.compile(r'(\]\()(<?)([^\s)>]+)(>?)([^)]*\))')
for path in dest.rglob('*.md'):
    relative = path.relative_to(dest)
    if any(part.startswith('_') for part in relative.parts):
        continue
    content = path.read_text(encoding='utf-8')
    def replace(match):
        url = urlsplit(match.group(3))
        if url.scheme or url.netloc or not url.path:
            return match.group(0)
        target = (source / relative.parent / url.path).resolve()
        if target.is_file() and target.suffix.lower() == '.md' and target.is_relative_to(source):
            newpath = url.path[:-3] + '.html'
        elif target.is_relative_to(root) and not target.is_relative_to(source) and target.exists():
            newpath = 'https://github.com/knotrel/knotrel/blob/main/' + target.relative_to(root).as_posix()
        else:
            return match.group(0)
        return match.group(1) + match.group(2) + urlunsplit(('', '', newpath, url.query, url.fragment)) + match.group(4) + match.group(5)
    content = pattern.sub(replace, content)
    if not content.startswith('---\n'):
        content = '---\nlayout: default\n---\n' + content
    path.write_text(content, encoding='utf-8')
