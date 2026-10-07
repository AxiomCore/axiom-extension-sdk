#!/usr/bin/env python3
"""Passive public-package release check using one-way source signatures.

This distributable checker contains no private source or build information.
It never runs a package, native executable or package lifecycle script.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tarfile
import tempfile
import zipfile
import zlib


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Rejected(Exception):
    pass


def private_match(data, policy):
    if sha(data) in policy['wholeFileSha256']:
        return True
    width = policy['windowBytes']
    if width != 256 or len(data) < width:
        return False
    signatures = {}
    for item in policy['windows']:
        signatures.setdefault(item['adler32'], set()).add(item['sha256'])
    value = zlib.adler32(data[:width]); a, b = value & 65535, value >> 16
    for offset in range(len(data) - width + 1):
        value = (b << 16) | a
        if value in signatures and sha(data[offset:offset + width]) in signatures[value]:
            return True
        if offset + width < len(data):
            old = data[offset]; new = data[offset + width]
            a = (a - old + new) % 65521
            b = (b - width * old + a - 1) % 65521
    return False


def name_ok(name):
    parts = name.rstrip('/').removeprefix('./').split('/')
    if not name or name.startswith('/') or '\\' in name or '\x00' in name or any(p in ('', '.', '..') for p in parts) or re.match('[A-Za-z]:', name):
        raise Rejected('unsafe container member')
    if any(p in ('.git', '.summary_files', 'audit-reports', 'axiom-internal-docs') or p == '.env' or p.startswith('.env.') or p.endswith('.dSYM') for p in parts) or name.endswith(('.map', '.pdb', '.dwo')) or '/release/control/' in '/' + name:
        raise Rejected('private or diagnostic payload')


def check(path, signatures, scanner, receipt):
    policy = json.loads(Path(signatures).read_text())
    if (policy.get('format') != 'axiom-public-artifact-signatures/v1' or not policy.get('windows')
            or not policy.get('wholeFileSha256') or policy.get('windowBytes') != 256):
        raise Rejected('invalid or empty inspection signatures')
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise Rejected('missing or linked artifact')
    members, total = [], 0
    with tempfile.TemporaryDirectory(prefix='artifact-check-') as temporary:
        scratch = Path(temporary) / 'payload'; scratch.mkdir(mode=0o700)
        def visit(name, data, depth=0):
            nonlocal total
            name_ok(name.split('!')[-1]); limits = policy['limits']
            total += len(data)
            if depth > limits['depth'] or len(members) >= limits['members'] or len(data) > limits['memberBytes'] or total > limits['totalBytes']:
                raise Rejected('inspection extraction limit exceeded')
            identity = sha(data)
            members.append({'member': name, 'sha256': identity, 'bytes': len(data)})
            (scratch / (identity + '.raw')).write_bytes(data)
            if b'\x00' in data[:8192]:
                runs = b'\n'.join(match.group().decode('utf-8', errors='replace').encode('utf-8')
                                  for match in re.finditer(rb'[\x09\x0a\x0d\x20-\x7e\x80-\xf4]{8,}', data))
                (scratch / (identity + '.utf8')).write_bytes(runs)
            if data.startswith((b'7z\xbc\xaf\x27\x1c', b'Rar!', b'BZh', b'\xfd7zXZ', b'\x28\xb5\x2f\xfd')):
                raise Rejected('unsupported archive')
            if data.startswith(b'PK') or name.endswith(('.zip', '.vsix', '.whl', '.apk')):
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    if len(archive.infolist()) > limits['members']:
                        raise Rejected('too many container entries')
                    seen = set()
                    metadata = archive.comment
                    for item in archive.infolist():
                        metadata += item.filename.encode('utf-8') + b'\n' + item.comment + item.extra
                        if len(metadata) > 1048576:
                            raise Rejected('container metadata too large')
                        name_ok(item.filename)
                        mode = item.external_attr >> 16
                        if item.filename in seen or item.flag_bits & 1 or stat.S_ISLNK(mode) or stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR):
                            raise Rejected('duplicate, encrypted or linked container member')
                        seen.add(item.filename)
                        if not item.is_dir():
                            if item.file_size > limits['memberBytes']:
                                raise Rejected('oversized container member')
                            with archive.open(item) as stream:
                                raw = stream.read(limits['memberBytes'] + 1)
                            if len(raw) != item.file_size:
                                raise Rejected('incomplete container extraction')
                            visit(name + '!' + item.filename, raw, depth + 1)
                    if metadata:
                        visit(name + '!@zip-metadata.txt', metadata, depth + 1)
                return
            if data.startswith(b'\x1f\x8b'):
                if private_match(data, policy):
                    raise Rejected('private source in container metadata')
                with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:
                    raw = stream.read(limits['memberBytes'] + 1)
                if len(raw) > limits['memberBytes']:
                    raise Rejected('oversized expanded container')
                tar(name, raw, depth)
                return
            if name.endswith('.tar') or len(data) > 262 and data[257:262] == b'ustar':
                tar(name, data, depth)
                return
            if not any(item['sha256'] == identity for item in policy['approvedPublicMembers']) and private_match(data, policy):
                raise Rejected('private source signature matched')
            if re.search(rb'/Users/[A-Za-z0-9_.-]+/|/Volumes/|/home/[A-Za-z0-9_.-]+/|[A-Z]:\\Users\\', data):
                raise Rejected('absolute build path in payload')
        def tar(name, data, depth):
            with tarfile.open(fileobj=io.BytesIO(data), mode='r:') as archive:
                seen = set()
                metadata = []
                for item in archive:
                    metadata.extend([item.name, '\n'.join(key + '\n' + value for key, value in sorted(item.pax_headers.items()))])
                    if sum(len(value.encode()) + 1 for value in metadata) > 1048576:
                        raise Rejected('container metadata too large')
                    name_ok(item.name)
                    if item.name in seen or len(seen) >= policy['limits']['members']:
                        raise Rejected('duplicate or excessive container entries')
                    seen.add(item.name)
                    if item.isdir():
                        continue
                    if not item.isfile() or item.size > policy['limits']['memberBytes']:
                        raise Rejected('linked, special or oversized container member')
                    stream = archive.extractfile(item)
                    raw = stream.read(policy['limits']['memberBytes'] + 1)
                    if len(raw) != item.size:
                        raise Rejected('incomplete container extraction')
                    visit(name + '!' + item.name, raw, depth + 1)
                if metadata:
                    visit(name + '!@tar-metadata.txt', '\n'.join(metadata).encode(), depth + 1)
        if path.stat().st_size > policy['limits']['memberBytes']:
            raise Rejected('oversized artifact')
        raw = path.read_bytes(); visit(path.name, raw)
        tool_sha = sha(Path(scanner).read_bytes())
        config = Path(signatures).with_name('gitleaks.toml')
        if not config.is_file() or sha(config.read_bytes()) != policy['gitleaksConfigSha256']:
            raise Rejected('credential scanner configuration missing or changed')
        version = subprocess.run([str(scanner), 'version'], check=True, capture_output=True, text=True, timeout=30).stdout.strip().removeprefix('v')
        if version != policy['gitleaksVersion'] or tool_sha not in policy['gitleaksBinarySha256']:
            raise Rejected('credential scanner not pinned by policy')
        output = Path(temporary) / 'secrets.json'
        result = subprocess.run([str(scanner), 'dir', str(scratch), '--config', str(config), '--report-format', 'json', '--report-path', str(output), '--redact=100', '--no-banner'], capture_output=True, timeout=300)
        if result.returncode not in (0, 1) or not output.is_file():
            raise Rejected('credential scanner failed')
        for finding in json.loads(output.read_text()):
            file = Path(finding['File'])
            if not file.is_absolute(): file = scratch / file
            if not file.resolve().is_relative_to(scratch.resolve()):
                raise Rejected('invalid credential result provenance')
            if not re.fullmatch('[0-9a-f]{64}\\.(raw|utf8)', file.name):
                raise Rejected('unknown credential scan view')
            original = scratch / (file.stem + '.raw')
            if not original.is_file() or sha(original.read_bytes()) != file.stem:
                raise Rejected('credential scan view lost payload binding')
            row = {'memberSha256': file.stem, 'scanViewSha256': sha(file.read_bytes()),
                   'scanView': file.suffix.removeprefix('.'), 'rule': finding['RuleID'], 'startLine': finding['StartLine'], 'endLine': finding['EndLine']}
            if not any(all(item.get(key) == value for key, value in row.items()) and item.get('reason') for item in policy['benignSecretFindings']):
                raise Rejected('credential pattern in payload')
    if sha(path.read_bytes()) != sha(raw):
        raise Rejected('artifact changed during inspection')
    report = {'format': 'axiom-public-package-inspection/v1', 'status': 'passed',
              'sourceHead': os.environ.get('GITHUB_SHA'), 'runId': os.environ.get('GITHUB_RUN_ID'),
              'artifact': path.name, 'artifactSha256': sha(raw), 'members': members,
              'signaturePolicySha256': sha(Path(signatures).read_bytes()),
              'checkerSha256': sha(Path(__file__).read_bytes()), 'scannerSha256': tool_sha}
    Path(receipt).write_text(json.dumps(report, indent=2) + '\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact', type=Path, required=True)
    parser.add_argument('--signatures', type=Path, required=True)
    parser.add_argument('--scanner', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    try:
        report = check(args.artifact, args.signatures, args.scanner, args.receipt)
        print(json.dumps({'status': report['status'], 'sha256': report['artifactSha256'], 'members': len(report['members'])}))
    except (Rejected, OSError, ValueError, KeyError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile, EOFError):
        raise SystemExit('Final package inspection failed; no package was executed. Review the private release evidence.')
