"""Check current crate introductions, excluding historical release documents."""
import pathlib
import re
import sys
import tomllib


def stale_references(text, version):
    """Return line numbers containing release numbers unlike the workspace version."""
    return [n for n, line in enumerate(text.splitlines(), 1)
            if any(match != version for match in re.findall(r'(?<![\w.])v?(\d+\.\d+\.\d+)(?![\w.]|-[0-9A-Za-z])', line))]


def unsupported_math(text):
    """Find LaTeX math delimiters unsupported by our plain Rustdoc rendering."""
    pattern = re.compile(r'(?<!\\)\$[^$\n]+(?<!\\)\$(?![\w{])|\\[()\[\]]')
    return [n for n, line in enumerate(text.splitlines(), 1) if pattern.search(line)]


def main():
    """Check crate README files and module-level Rustdoc introductions."""
    root = pathlib.Path(__file__).resolve().parents[1]
    version = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']
    errors = []
    for crate in sorted((root / 'crates').iterdir()):
        if not crate.is_dir():
            continue
        for path in [crate / 'README.md', crate / 'src/lib.rs']:
            if not path.exists():
                continue
            text = path.read_text()
            if path.suffix == '.rs':
                text = '\n'.join(line if line.startswith('//!') else '' for line in text.splitlines())
            errors.extend(f'{path.relative_to(root)}:{n}: review stale version reference (current {version})'
                          for n in stale_references(text, version))
    paths = list((root / 'crates').glob('*/README.md')) + list((root / 'crates').glob('*/src/**/*.rs'))
    for path in sorted(paths):
        if not path.is_file() or not (path.name == 'README.md' or path.suffix == '.rs'):
            continue
        text = path.read_text()
        if path.suffix == '.rs':
            text = '\n'.join(line if line.lstrip().startswith(('///', '//!')) else ''
                             for line in text.splitlines())
        errors.extend(f'{path.relative_to(root)}:{n}: unsupported LaTeX math; use inline code notation'
                      for n in unsupported_math(text))
    print('\n'.join(errors) if errors else 'Crate introduction version checks passed.')
    return bool(errors)


if __name__ == '__main__':
    sys.exit(main())
