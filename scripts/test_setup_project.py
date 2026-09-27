import importlib.util
from pathlib import Path
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("setup_project", Path(__file__).with_name("setup_project.py"))
assert SPEC and SPEC.loader
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class BoundaryTests(unittest.TestCase):
    def test_scalar_state_fields_are_visible(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "main.acore").write_text(
                'page Main {\n  state cart {\n    subtotal: Int = 10\n    label: String = "x"\n  }\n}\n'
            )
            types = module.acore_boundary(root, {
                "permissions": {"ui-state": {"cart": {"read": ["subtotal"], "write": ["label"]}}}
            })
            self.assertEqual(types, {"cart": {"subtotal": "Int", "label": "String"}})

    def test_missing_local_field_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "main.acore").write_text("page Main {\n  state cart {\n    subtotal: Int = 10\n  }\n}\n")
            with self.assertRaisesRegex(RuntimeError, "cart.discount"):
                module.acore_boundary(root, {
                    "permissions": {"ui-state": {"cart": {"write": ["discount"]}}}
                })

    def test_external_scope_is_not_guessed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "main.acore").write_text("page Main {\n  state subtotal: Int = 10\n}\n")
            self.assertEqual(module.acore_boundary(root, {
                "permissions": {"ui-state": {"host_cart": {"read": ["total"]}}}
            }), {})


if __name__ == "__main__":
    unittest.main()
