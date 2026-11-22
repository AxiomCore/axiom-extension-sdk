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


class NoImplicitConversion(unittest.TestCase):
    """Verifier findings M-2 / L-4 (unit w1-f2): the Python side of the
    coercion hole. Every implicit use fails loudly; explicit forms are exact."""

    def test_ordering_against_other_types_is_a_type_error(self) -> None:
        price = Cents(5)
        rate = Percentage.from_whole(5)
        for left, right in ((price, 5), (5, price), (price, rate), (rate, 500), (rate, price), (price, "5")):
            for compare in (lambda a, b: a < b, lambda a, b: a <= b, lambda a, b: a > b, lambda a, b: a >= b):
                with self.assertRaises(TypeError):
                    compare(left, right)
        self.assertTrue(Cents(4) < price <= Cents(5) < Cents(1 << 63))
        self.assertTrue(Percentage(1) < rate)

    def test_no_arithmetic_with_int_or_between_values(self) -> None:
        price = Cents(5)
        for operation in (lambda: price + 1, lambda: 1 + price, lambda: price + price, lambda: price - Cents(1),
                          lambda: price * 2, lambda: price // 2, lambda: -price, lambda: int(price),
                          lambda: range(price), lambda: Percentage(1) + 1, lambda: int(Percentage(1))):  # type: ignore[operator, arg-type, call-overload]
            with self.assertRaises(TypeError):
                operation()
        self.assertEqual(price.checked_sub(Cents(1)), Cents(4))

    def test_truth_value_is_a_type_error(self) -> None:
        for value in (Cents.ZERO, Cents(5), Percentage(0), Percentage.from_whole(1)):
            with self.assertRaisesRegex(TypeError, "has no truth value"):
                bool(value)
            with self.assertRaises(TypeError):
                if value:  # noqa: SIM108
                    pass
        self.assertTrue(Cents(5) != Cents.ZERO)
        self.assertEqual(Cents.ZERO.value, 0)

    def test_equality_and_hash_are_consistent(self) -> None:
        self.assertNotEqual(Cents(5), 5)
        self.assertNotEqual(5, Cents(5))
        self.assertNotEqual(Percentage(5), Cents(5))
        self.assertEqual({Cents(5): "a", Cents(5): "b"}, {Cents(5): "b"})
        self.assertEqual(len({Cents(5), 5}), 2)
        self.assertEqual(hash(Percentage.from_whole(1)), hash(Percentage(100)))

    def test_json_dumps_refuses_values(self) -> None:
        for value in (Cents(5), Percentage(5), {"price": Cents(5)}, [Percentage(1)]):
            with self.assertRaises(TypeError):
                json.dumps(value)
        self.assertEqual(json.dumps({"price": Cents(5).encode(), "rate": Percentage(5).encode()}), '{"price": 5, "rate": 5}')

    def test_wrong_operand_types_are_type_errors(self) -> None:
        for operation in (lambda: Cents(5).min(5), lambda: Cents(5).checked_sub(1),  # type: ignore[arg-type]
                          lambda: Percentage(5).of(100)):  # type: ignore[arg-type]
            with self.assertRaisesRegex(TypeError, "^expected Cents$"):
                operation()

    def test_instances_are_immutable(self) -> None:
        for value, field in ((Cents(5), "_cents"), (Percentage(5), "_basis_points")):
            with self.assertRaises(AttributeError):
                setattr(value, field, 1)
            with self.assertRaises(AttributeError):
                delattr(value, field)
        self.assertEqual(Cents(5).value, 5)
        self.assertEqual(Percentage(5).basis_points, 5)

    def test_constructor_takes_basis_points(self) -> None:
        self.assertEqual(Percentage(50), Percentage.from_basis_points(50))
        self.assertEqual(Percentage(50).of(Cents(10_000)), Cents(50))
        self.assertEqual(Percentage.from_whole(50).of(Cents(10_000)), Cents(5_000))

    def test_package_root_reexports_the_money_names(self) -> None:
        import axiom_extension_sdk

        self.assertIs(axiom_extension_sdk.Cents, Cents)
        self.assertIs(axiom_extension_sdk.Percentage, Percentage)
        stub = (ROOT / "python" / "axiom_extension_sdk" / "__init__.pyi").read_text(encoding="utf-8")
        self.assertIn("from .money import Cents as Cents, Percentage as Percentage", stub)


if __name__ == "__main__":
    unittest.main()
