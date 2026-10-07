import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

import check_release_artifact as checker
import publish_reviewed_crate as publish
import verify_inspected_packages as verify


class ReleaseSecurityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.archives = self.root / 'archives'; self.archives.mkdir()
        self.reports = self.root / 'reports'; self.reports.mkdir()
        self.signatures = Path(__file__).resolve().parents[1] / '.github/security/signatures.json'

    def tearDown(self):
        self.temp.cleanup()

    def crate(self):
        output = io.BytesIO()
        manifest = b'[package]\nname="example-sdk"\nversion="1.2.3"\nlicense="Apache-2.0"\nreadme="README.md"\n[dependencies]\nwire={package="serde",version="1",default-features=false,features=["derive"]}\n[target.\'cfg(unix)\'.build-dependencies]\ncc="1"\n'
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, data in [('Cargo.toml', manifest), ('README.md', b'Public SDK'), ('src/lib.rs', b'pub struct Public;')]:
                item = tarfile.TarInfo('example-sdk-1.2.3/' + name); item.size = len(data)
                archive.addfile(item, io.BytesIO(data))
        return output.getvalue()

    def receipt(self, path):
        policy = json.loads(self.signatures.read_text())
        record = {'format': 'axiom-public-package-inspection/v1', 'status': 'passed',
            'artifact': path.name, 'artifactSha256': checker.sha(path.read_bytes()),
            'signaturePolicySha256': checker.sha(self.signatures.read_bytes()),
            'checkerSha256': checker.sha(Path(checker.__file__).read_bytes()),
            'scannerSha256': policy['gitleaksBinarySha256'][0], 'sourceHead': 'a' * 40,
            'members': [{'member': path.name, 'sha256': checker.sha(path.read_bytes())}]}
        target = self.reports / (path.name + '.json'); target.write_text(json.dumps(record))
        return target

    def test_receipts_bind_payload_policy_checker_and_source(self):
        path = self.archives / 'example.tgz'; path.write_bytes(b'public package')
        receipt = self.receipt(path)
        self.assertEqual(verify.verify(self.archives, self.reports, self.signatures, 'a' * 40), {path.name: checker.sha(path.read_bytes())})
        with self.assertRaises(ValueError): verify.verify(self.archives, self.reports, self.signatures, 'b' * 40)
        path.write_bytes(b'mutated')
        with self.assertRaises(ValueError): verify.verify(self.archives, self.reports, self.signatures)
        path.write_bytes(b'public package'); receipt.unlink()
        with self.assertRaises(ValueError): verify.verify(self.archives, self.reports, self.signatures)

    def test_exact_crate_upload_uses_reviewed_bytes_without_rebuild(self):
        path = self.archives / 'example-sdk-1.2.3.crate'; data = self.crate(); path.write_bytes(data)
        receipt = self.receipt(path); requests = []
        class Opener:
            def open(self, request, timeout):
                requests.append(request)
                return io.BytesIO(b'{"warnings":{}}')
        result = publish.upload(path, receipt, self.signatures, '1.2.3', 'unit-fixture-token', Opener())
        self.assertEqual(result['sha256'], checker.sha(data))
        request = requests[0]
        self.assertEqual(request.full_url, 'https://crates.io/api/v1/crates/new')
        self.assertEqual(request.method, 'PUT')
        length = struct.unpack_from('<I', request.data)[0]
        metadata = json.loads(request.data[4:4 + length]); size = struct.unpack_from('<I', request.data, 4 + length)[0]
        self.assertEqual(size, len(data)); self.assertEqual(request.data[8 + length:], data)
        self.assertEqual(metadata['deps'][0]['name'], 'serde')
        self.assertEqual(metadata['deps'][0]['explicit_name_in_toml'], 'wire')
        self.assertEqual(metadata['deps'][0]['version_req'], '^1')
        self.assertFalse(metadata['deps'][0]['default_features'])
        self.assertEqual(metadata['deps'][1]['target'], 'cfg(unix)')

    def test_mutated_crate_wrong_version_or_missing_token_never_contacts_registry(self):
        path = self.archives / 'example-sdk-1.2.3.crate'; path.write_bytes(self.crate()); receipt = self.receipt(path)
        class Opener:
            def open(self, *args, **kwargs): raise AssertionError('unexpected registry request')
        with self.assertRaises(ValueError): publish.upload(path, receipt, self.signatures, '9.9.9', 'unit-fixture-token', Opener())
        with self.assertRaises(ValueError): publish.upload(path, receipt, self.signatures, '1.2.3', None, Opener())
        path.write_bytes(path.read_bytes() + b'changed')
        with self.assertRaises(ValueError): publish.upload(path, receipt, self.signatures, '1.2.3', 'unit-fixture-token', Opener())

    def test_credentials_cannot_redirect_to_another_host(self):
        with self.assertRaises(ValueError):
            publish.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://another.invalid')


if __name__ == '__main__':
    unittest.main()
