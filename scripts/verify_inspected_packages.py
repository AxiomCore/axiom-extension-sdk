#!/usr/bin/env python3
"""Check final downloaded bytes against successful package inspections."""
import argparse
import json
import os
from pathlib import Path

from check_release_artifact import sha


def verify(archives, receipts, signatures, source_head=None):
    paths = sorted(Path(archives).iterdir())
    reports = sorted(Path(receipts).iterdir())
    policy = json.loads(Path(signatures).read_text())
    if (not paths or any(p.is_symlink() or not p.is_file() for p in paths + reports)
            or {p.name + '.json' for p in paths} != {p.name for p in reports}):
        raise ValueError('incomplete or unexpected package/inspection inventory')
    result = {}
    checker = Path(__file__).with_name('check_release_artifact.py')
    for path in paths:
        report = json.loads((Path(receipts) / (path.name + '.json')).read_text())
        identity = sha(path.read_bytes())
        if (report.get('format') != 'axiom-public-package-inspection/v1' or report.get('status') != 'passed'
                or report.get('artifact') != path.name or report.get('artifactSha256') != identity
                or report.get('signaturePolicySha256') != sha(Path(signatures).read_bytes())
                or report.get('checkerSha256') != sha(checker.read_bytes())
                or report.get('scannerSha256') not in policy['gitleaksBinarySha256']
                or not report.get('members') or report['members'][0].get('sha256') != identity
                or source_head is not None and report.get('sourceHead') != source_head):
            raise ValueError('package inspection missing, stale or hash-mismatched')
        result[path.name] = identity
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archives', type=Path, required=True)
    parser.add_argument('--receipts', type=Path, required=True)
    parser.add_argument('--signatures', type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.archives, args.receipts, args.signatures, os.environ.get('GITHUB_SHA'))))
    except (OSError, ValueError, KeyError):
        raise SystemExit('Public package inspection check failed before publication.')
