import assert from "node:assert/strict";
import test from "node:test";
import { authenticatedRequest, decodeValue, encodeValue } from "../dist/index.js";

// The managed bridge carries integers outside the JS safe range as exact
// decimal strings. The SDK must surface them as bigint and never round.
test("64-bit integers decode to exact bigint and re-encode losslessly", () => {
  assert.equal(decodeValue({ Unsigned: "18446744073709551615" }), 18446744073709551615n);
  assert.equal(decodeValue({ Signed: "-9223372036854775808" }), -9223372036854775808n);
  assert.equal(decodeValue({ Unsigned: 42 }), 42);
  assert.deepEqual(encodeValue(18446744073709551615n), { Unsigned: "18446744073709551615" });
  assert.deepEqual(encodeValue(-9223372036854775808n), { Signed: "-9223372036854775808" });
  assert.deepEqual(encodeValue(5n), { Unsigned: 5 });
  assert.deepEqual(encodeValue(decodeValue({ Unsigned: "9007199254740993" })), { Unsigned: "9007199254740993" });
});

test("integers outside 64 bits and unsafe numbers are rejected, not rounded", () => {
  assert.throws(() => encodeValue(2n ** 64n), /64 bits/);
  assert.throws(() => encodeValue(-(2n ** 63n) - 1n), /64 bits/);
  assert.throws(() => encodeValue(9007199254740993), /safe integers/);
});

test("opaque secret handles with large IDs survive; malformed IDs do not", () => {
  const binding = { namespace: "network", operation: "send" };
  const handle = { id: "9007207844675585", generation: 1, kind: "Secret" };
  const effect = authenticatedRequest(binding, "/pay", [], handle);
  assert.deepEqual(effect.input.Record.find((field) => field.name === "secret").value, { Handle: handle });
  for (const id of ["007", "", "1e3", 1.5, -1]) {
    assert.throws(() => authenticatedRequest(binding, "/pay", [], { ...handle, id }), /unavailable/);
  }
});
