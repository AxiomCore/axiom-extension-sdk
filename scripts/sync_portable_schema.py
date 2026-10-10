#!/usr/bin/env python3
"""Bind the generated SDK helper to reviewed host source without a CI sibling checkout."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PREFIX = b'// @ts-nocheck\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host-source', type=Path, required=True)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    source = args.host_source.read_bytes()
    generated = PREFIX + source
    manifest = {
        'format': 'axiom-portable-schema-source/v1',
        'sourcePath': 'axiom-ui-host/web/acore-schema.js',
        'sourceSha256': hashlib.sha256(source).hexdigest(),
        'generatedSha256': hashlib.sha256(generated).hexdigest(),
    }
    outputs = {
        ROOT / 'typescript/src/schema-runtime.ts': generated,
        ROOT / 'typescript/schema-runtime.source.json': (json.dumps(manifest, indent=2)+'\n').encode(),
    }
    for path, expected in outputs.items():
        if args.check:
            if not path.is_file() or path.read_bytes() != expected:
                raise SystemExit('Portable schema drift: '+str(path.relative_to(ROOT)))
        else:
            path.write_bytes(expected)
    print('Portable schema source binding '+('checked' if args.check else 'generated'))


if __name__ == '__main__':
    main()
