"""Editor-friendly import shim for Axiom Python extensions.

The Axiom compiler parses authored source; it never imports this module while
building or running an extension. The host supplies the real context.
"""

from typing import Any, Callable, Generic, Protocol, TypeVar

T = TypeVar("T", bound=Callable[..., object])
ValueT = TypeVar("ValueT")


class Selector(str, Generic[ValueT]):
    """A generated, permission-scoped state path with an editor-visible value type."""


class Snapshot(Protocol):
    def get(self, path: Selector[ValueT]) -> ValueT | None: ...


class Patch(Protocol):
    def set(self, path: Selector[ValueT], value: ValueT) -> "Patch": ...
    def unset(self, path: Selector[Any]) -> "Patch": ...
    def increment(self, path: Selector[int], amount: int) -> "Patch": ...
    def build(self) -> Any: ...


class SecretHandle(Protocol):
    """Opaque invocation-scoped capability; no material/reveal API."""
    @property
    def kind(self) -> str: ...

class Context(Protocol):
    input: Any
    def secret(self, alias: str) -> SecretHandle: ...
    def authenticated_request(self, namespace: str, operation: str, path: str, body: bytes | list[int], secret: SecretHandle) -> Any: ...
    def effects(self, *effects: Any) -> Any: ...
    def snapshot(self, resource: str) -> Snapshot: ...
    def patch(self, resource: str) -> Patch: ...
    def complete(self, output: Any = ..., *, patches: list[Any] = ...,
                 transactions: list[Any] = ..., emitted_events: list[Any] = ...) -> Any: ...


class ResumeContext(Protocol):
    outcomes: list[Any]
    events: list[Any]
    def outcome(self, index: int) -> Any: ...
    def complete(self, output: Any = ...) -> Any: ...

class _Extension:
    def export(self, function: T) -> T:
        return function
    def resume(self, function: T) -> T:
        return function


extension = _Extension()
