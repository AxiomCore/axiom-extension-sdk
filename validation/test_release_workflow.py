"""The publisher's artifacts must not dirty its committed source checkout."""
import importlib.util
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ReleaseWorkflowTests(unittest.TestCase):
    def test_every_artifact_transfer_uses_runner_temp_outside_checkout(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        paths = re.findall(r"^          path: (.+)$", workflow, re.MULTILINE)
        self.assertEqual(len(paths), 6)
        self.assertTrue(all(path.startswith("${{ runner.temp }}/axiom-extension-release/") for path in paths))
        self.assertNotIn('mkdir -p release-archives', workflow)
        self.assertIn('--no-create-gitignore', workflow)
        # Syntax-check every actual shell block, without executing any upload.
        blocks = re.findall(r"^        run: \|\n((?:          .*\n|\n)+)", workflow, re.MULTILINE)
        self.assertEqual(len(blocks), 4)
        for block in blocks:
            subprocess.run(['bash', '-n'], input='\n'.join(line[10:] for line in block.splitlines()),
                           text=True, capture_output=True, check=True)

    def test_external_artifacts_preserve_clean_source_guard_and_dirty_source_still_fails(self):
        spec = importlib.util.spec_from_file_location('release_input', ROOT / 'scripts/verify_release_input.py')
        helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            checkout = base / 'checkout'; checkout.mkdir()
            subprocess.run(['git', 'init', '-q', str(checkout)], check=True)
            manifest = checkout / 'rust/abi/Cargo.toml'; manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\nname="axiom-extension-abi"\nversion="1.0.3"\n')
            subprocess.run(['git', 'add', '.'], cwd=checkout, check=True)
            subprocess.run(['git', '-c', 'user.name=Workflow Test', '-c', 'user.email=test@example.invalid',
                            'commit', '-qm', 'Committed input'], cwd=checkout, check=True)
            archives = base / 'runner-temp/axiom-extension-release/archives'; archives.mkdir(parents=True)
            (archives / 'package.crate').write_bytes(b'reviewed external artifact')
            helper.ROOT = checkout
            helper.main('rust-abi', '1.0.3')
            (checkout / 'unexpected-file').write_text('dirty input')
            with self.assertRaisesRegex(SystemExit, 'clean and committed'):
                helper.main('rust-abi', '1.0.3')
            (checkout / 'unexpected-file').unlink()
            manifest.write_text(manifest.read_text() + '# uncommitted change\n')
            with self.assertRaisesRegex(SystemExit, 'clean and committed'):
                helper.main('rust-abi', '1.0.3')


if __name__ == '__main__':
    unittest.main()
