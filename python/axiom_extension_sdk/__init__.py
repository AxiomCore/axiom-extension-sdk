"""Editor-friendly import shim for Axiom Python extensions.

The Axiom compiler parses authored source; it never imports this module while
building or running an extension. The host supplies the real context.
"""

from typing import Any, Callable, Generic, Mapping, Protocol, TypedDict, TypeVar, overload

T = TypeVar("T", bound=Callable[..., object])
ValueT = TypeVar("ValueT")
O = TypeVar("O")


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

class TypedEffect(TypedDict, Generic[O]):
    """An effect whose outcome decodes as ``O``. The wire shape is unchanged:
    ``{"namespace": ..., "operation": ..., "input": ...}``."""
    namespace: str
    operation: str
    input: Any


class Ticket(Generic[O]):
    """A typed claim on the outcome of one host call in one effect plan.

    Issued only by ``Effects.push`` (the host's plan object); ``index`` is the
    ABI outcome index. Read it with ``ResumeContext.get``. In the AOT profile
    these names exist for annotations only: import them under
    ``if TYPE_CHECKING:``.
    """

    @property
    def index(self) -> int:
        raise NotImplementedError("tickets are issued by the host's Effects plan")


class Effects(Protocol):
    """An effect plan built call by call; yield it with ``ctx.effects(plan)``.

    ``plan.push(effect)`` appends a host call and returns its ticket (the
    AOT host bridge never exposes a member named ``call``).

    The yielded plan is byte-identical to ``ctx.effects(*effects)``.
    """
    @overload
    def push(self, effect: TypedEffect[ValueT]) -> Ticket[ValueT]: ...
    @overload
    def push(self, effect: Mapping[str, Any]) -> Ticket[Any]: ...
    @property
    def length(self) -> int: ...


class Context(Protocol):
    input: Any
    def secret(self, alias: str) -> SecretHandle: ...
    def authenticated_request(self, namespace: str, operation: str, path: str, body: bytes | list[int], secret: SecretHandle) -> Any: ...
    def effects(self, *effects: Any) -> Any: ...
    def plan(self) -> Effects: ...
    def snapshot(self, resource: str) -> Snapshot: ...
    def patch(self, resource: str) -> Patch: ...
    def complete(self, output: Any = ..., *, patches: list[Any] = ...,
                 transactions: list[Any] = ..., emitted_events: list[Any] = ...) -> Any: ...


class ResumeContext(Protocol):
    outcomes: list[Any]
    events: list[Any]
    def get(self, ticket: Ticket[ValueT]) -> ValueT:
        """Decode the outcome of the host call that issued ``ticket``.

        A ticket from another plan or yield step raises."""
        ...
    def plan(self) -> Effects: ...
    def outcome(self, index: int) -> Any:
        """Deprecated: positional outcome access.

        Use ``ctx.get(ticket)`` with a ticket from ``plan.push(effect)``. Kept
        working. No ``DeprecationWarning`` is emitted: the context is supplied
        by the host runtime, not by this module."""
        ...
    def complete(self, output: Any = ...) -> Any: ...

class _Extension:
    def export(self, function: T) -> T:
        return function
    def resume(self, function: T) -> T:
        return function


extension = _Extension()
