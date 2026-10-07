#!/usr/bin/env python3
"""Upload the exact inspected crate, without rebuilding or repacking it.

Uses the Cargo registry publish API. The token stays in the environment and
Authorization header; errors never include headers or archive/source content.
"""
import argparse
import io
import json
import os
from pathlib import Path
import re
import struct
import tarfile
import tomllib
import urllib.error
import urllib.request

from check_release_artifact import sha


def crate_metadata(data):
    files = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        for item in archive:
            if item.isdir():
                continue
            parts = item.name.split('/')
            if not item.isfile() or len(parts) < 2 or any(p in ('', '.', '..') for p in parts) or item.size > 16 * 1024 * 1024:
                raise ValueError('invalid reviewed crate member')
            key = '/'.join(parts[1:])
            if key in files:
                raise ValueError('duplicate reviewed crate member')
            files[key] = archive.extractfile(item).read()
    manifest = tomllib.loads(files['Cargo.toml'].decode())
    package = manifest['package']
    if not re.fullmatch('[A-Za-z0-9_-]+', package['name']) or not re.fullmatch(r'\d+\.\d+\.\d+', package['version']):
        raise ValueError('invalid reviewed crate identity')
    dependencies = []
    tables = [(manifest, None)] + [(table, target) for target, table in manifest.get('target', {}).items()]
    for table, target in tables:
        for section, kind in [('dependencies', 'normal'), ('build-dependencies', 'build'), ('dev-dependencies', 'dev')]:
            for name, value in table.get(section, {}).items():
                if isinstance(value, str): value = {'version': value}
                if any(key in value for key in ('git', 'path', 'registry', 'workspace')) or 'version' not in value:
                    raise ValueError('unsupported dependency in reviewed crate')
                requirement = value['version']
                if re.match(r'^\d', requirement): requirement = '^' + requirement
                dependencies.append({'name': value.get('package', name), 'version_req': requirement,
                    'features': value.get('features', []), 'optional': value.get('optional', False),
                    'default_features': value.get('default-features', True), 'target': target,
                    'kind': kind, 'registry': None,
                    'explicit_name_in_toml': name if value.get('package') else None})
    readme = package.get('readme')
    if readme is True: readme = 'README.md'
    result = {'name': package['name'], 'vers': package['version'], 'deps': dependencies,
              'features': manifest.get('features', {}), 'authors': package.get('authors', []),
              'keywords': package.get('keywords', []), 'categories': package.get('categories', []),
              'badges': manifest.get('badges', {}), 'readme_file': readme if isinstance(readme, str) else None,
              'readme': files[readme].decode() if isinstance(readme, str) else None,
              'license_file': package.get('license-file'), 'rust_version': package.get('rust-version')}
    for field in ('description', 'documentation', 'homepage', 'license', 'repository', 'links'):
        result[field] = package.get(field)
    return result


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        raise ValueError('registry authorization redirects are prohibited')


def upload(archive, receipt, signatures, version, token, opener=None):
    data = Path(archive).read_bytes()
    record = json.loads(Path(receipt).read_text())
    if (record.get('format') != 'axiom-public-package-inspection/v1' or record.get('status') != 'passed'
            or record.get('artifactSha256') != sha(data)
            or record.get('signaturePolicySha256') != sha(Path(signatures).read_bytes())):
        raise ValueError('crate inspection is missing, changed or stale')
    if not token:
        raise ValueError('registry authorization unavailable')
    metadata = crate_metadata(data)
    if metadata['vers'] != version or Path(archive).name != metadata['name'] + '-' + version + '.crate':
        raise ValueError('reviewed crate version differs from release input')
    encoded = json.dumps(metadata, separators=(',', ':')).encode()
    body = struct.pack('<I', len(encoded)) + encoded + struct.pack('<I', len(data)) + data
    request = urllib.request.Request('https://crates.io/api/v1/crates/new', data=body, method='PUT',
        headers={'Authorization': token, 'Content-Type': 'application/octet-stream',
                 'Accept': 'application/json', 'User-Agent': 'axiom-reviewed-crate-publisher/1.0'})
    opener = opener or urllib.request.build_opener(NoRedirect())
    try:
        with opener.open(request, timeout=120) as response:
            result = json.load(response)
        if result.get('errors'):
            raise ValueError('registry rejected reviewed crate')
    except urllib.error.URLError as error:
        raise ValueError('registry request failed; exact archive was not rebuilt') from error
    return {'package': metadata['name'], 'version': version, 'sha256': sha(data)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--signatures', type=Path, required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(upload(args.archive, args.receipt, args.signatures, args.version,
                                os.environ.get('CARGO_REGISTRY_TOKEN'))))
    except (OSError, ValueError, KeyError, tarfile.TarError):
        raise SystemExit('Exact crate publication failed; review the protected release job.')
