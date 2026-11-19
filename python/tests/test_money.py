"""Replays the Rust-generated X7e vectors against the Python helpers.

Run with CPython 3.12: python3.12 -I python/tests/test_money.py
"""
import json
import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))

from axiom_extension_sdk.money import Cents, Percentage  # noqa: E402

VECTORS = json.loads((ROOT / "validation" / "x7e-money-vectors.json").read_text(encoding="utf-8"))

OPS = {
    "cents.new": lambda v: str(Cents.new(v).value),
    "cents.encode": lambda v: str(Cents.new(v).encode()),
    "cents.decode": lambda v: str(Cents.decode(v).value),
    "cents.from_dollars": lambda d: str(Cents.from_dollars(d).value),
    "cents.checked_sub": lambda a, b: str(Cents.new(a).checked_sub(Cents.new(b)).value),
    "cents.min": lambda a, b: str(Cents.new(a).min(Cents.new(b)).value),
    "cents.as_currency": lambda v: Cents.new(v).as_currency(),
    "cents.as_saving": lambda v: Cents.new(v).as_saving(),
    "percentage.from_whole": lambda v: str(Percentage.from_whole(v).basis_points),
    "percentage.from_basis_points": lambda v: str(Percentage.from_basis_points(v).basis_points),
    "percentage.decode": lambda v: str(Percentage.decode(v).basis_points),
    "percentage.encode": lambda v: str(Percentage.from_basis_points(v).encode()),
    "percentage.of": lambda bp, amount: str(Percentage.from_basis_points(bp).of(Cents.new(amount)).value),
}


class RustVectors(unittest.TestCase):
    def test_every_vector_matches_rust(self) -> None:
        self.assertEqual(VECTORS["format"], "axiom-x7e-money-vectors/v1")
        self.assertGreaterEqual(len(VECTORS["cases"]), 90)
        seen = set()
        for case in VECTORS["cases"]:
            seen.add(case["op"])
            args = [int(a) for a in case["args"]]
            label = f"{case['op']}({', '.join(case['args'])})"
            with self.subTest(label):
                if "ok" in case:
                    self.assertEqual(OPS[case["op"]](*args), case["ok"])
                else:
                    with self.assertRaises(ValueError) as raised:
                        OPS[case["op"]](*args)
                    self.assertEqual(str(raised.exception), case["error"])
        self.assertEqual(sorted(seen), sorted(OPS))


class Types(unittest.TestCase):
    def test_wrong_types_are_type_errors(self) -> None:
        for bad in (True, 1.0, "5", None):
            with self.assertRaises(TypeError):
                Cents(bad)  # type: ignore[arg-type]
            with self.assertRaises(TypeError):
                Percentage.from_whole(bad)  # type: ignore[arg-type]
        with self.assertRaises(ValueError):
            Cents(-1)
        with self.assertRaises(ValueError):
            Cents(1 << 64)
        with self.assertRaisesRegex(TypeError, "^expected unsigned integer, received signed integer$"):
            Cents.decode(-1)
        with self.assertRaisesRegex(TypeError, "^expected unsigned integer, received string$"):
            Cents.decode("12")

    def test_immutable_hashable_ordered(self) -> None:
        price = Cents(1999)
        with self.assertRaises(AttributeError):
            price._cents = 1  # type: ignore[misc]
        self.assertEqual(str(price), "$19.99")
        self.assertEqual(price, Cents.new(1999))
        self.assertEqual(len({price, Cents(1999)}), 1)
        self.assertLess(Cents.ZERO, price)
        self.assertIs(price.min(Cents.ZERO), Cents.ZERO)
        self.assertEqual(Percentage.from_whole(25), Percentage.from_basis_points(2500))
        self.assertNotEqual(Cents(5), 5)


if __name__ == "__main__":
    unittest.main()
