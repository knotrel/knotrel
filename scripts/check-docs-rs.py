"""Read exact-version docs.rs build status; never trigger publication or rebuilds."""
import argparse
import json
import re
import urllib.error
import urllib.request


def classify(data):
    """Distinguish successful, failed and not-yet-known build status."""
    status = data.get('rustdoc_status')
    if status is True:
        return 'ready'
    if status is False:
        return 'failed'
    return 'pending/unknown'


def main():
    """Check both published crates once, leaving queue polling to a later rerun."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('version')
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', args.version):
        parser.error('expected an exact release version')
    outcomes = []
    for crate in ('knotrel-core', 'knotrel-server'):
        url = f'https://docs.rs/crate/{crate}/{args.version}/status.json'
        try:
            request = urllib.request.Request(url, headers={'User-Agent': 'knotrel-release-doc-check'})
            with urllib.request.urlopen(request, timeout=30) as response:
                data = json.load(response)
            status = classify(data) if isinstance(data, dict) else 'pending/unknown'
        except (urllib.error.URLError, TimeoutError, ValueError) as error:
            status = 'pending/unknown'
            print(f'{crate}: status lookup unavailable: {error}')
        outcomes.append(status)
        print(f'{crate} {args.version}: {status}; https://docs.rs/crate/{crate}/{args.version}/builds')
    return 1 if 'failed' in outcomes else (2 if 'pending/unknown' in outcomes else 0)


if __name__ == '__main__':
    raise SystemExit(main())
