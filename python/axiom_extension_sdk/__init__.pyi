from typing import Any, Awaitable, Callable, Generic, Mapping, Protocol, TypedDict, TypeVar, overload

from .money import Cents as Cents, Percentage as Percentage

T = TypeVar("T", bound=Callable[..., Any])
ValueT = TypeVar("ValueT")
O = TypeVar("O")

class Selector(str, Generic[ValueT]): ...

class Snapshot(Protocol):
    def get(self, path: Selector[ValueT]) -> ValueT | None: ...

class Patch(Protocol):
    def set(self, path: Selector[ValueT], value: ValueT) -> "Patch": ...
    def unset(self, path: Selector[Any]) -> "Patch": ...
    def increment(self, path: Selector[int], amount: int) -> "Patch": ...
    def build(self) -> Any: ...

class SecretHandle(Protocol):
    """Opaque capability, scoped to the current invocation."""
    @property
    def kind(self) -> str: ...

class TypedEffect(TypedDict, Generic[O]):
    """An effect whose outcome decodes as ``O`` (wire shape unchanged)."""
    namespace: str
    operation: str
    input: Any

def typed_effect(namespace: str, operation: str, input: Any, output: type[O]) -> TypedEffect[O]:
    """An effect whose outcome decodes as ``output``: ``plan.push`` of it gives
    a ``Ticket[output]``. Returns the plain effect dict (``input`` in wire
    form, as for a hand-written effect); ``output`` is used for typing only."""
    ...

class Ticket(Generic[O]):
    """Typed claim on one host call's outcome in one effect plan.

    Issued only by ``Effects.push``; ``index`` is the ABI outcome index. A
    resume rebuilds the yielded plan with the same calls (namespace,
    operation and input) in the same order; ``ResumeContext.get`` checks
    that against the host's record of the yielded plan.
    """
    @property
    def index(self) -> int: ...

class Effects(Protocol):
    """An effect plan built call by call; yield it with ``ctx.effects(plan)``.

    ``plan.push(effect)`` appends a host call and returns its ticket (the
    AOT host bridge never exposes a member named ``call``)."""
    @overload
    def push(self, effect: TypedEffect[O]) -> Ticket[O]: ...
    @overload
    def push(self, effect: Mapping[str, Any]) -> Ticket[Any]: ...
    @property
    def length(self) -> int: ...

class Context(Protocol):
    input: Any
    def secret(self, alias: str) -> SecretHandle: ...
    def authenticated_request(self, namespace: str, operation: str, path: str, body: bytes | list[int], secret: SecretHandle) -> Any: ...
    def effects(self, *effects: Any) -> Any:
        """Yield ``ctx.effects(plan)`` for an ``Effects`` plan, or raw effects."""
        ...
    def plan(self) -> Effects: ...
    def snapshot(self, resource: str) -> Snapshot: ...
    def patch(self, resource: str) -> Patch: ...
    def complete(
        self,
        output: Any = ...,
        *,
        patches: list[Any] = ...,
        transactions: list[Any] = ...,
        emitted_events: list[Any] = ...,
    ) -> Any: ...

class ResumeContext(Protocol):
    outcomes: list[Any]
    events: list[Any]
    def get(self, ticket: Ticket[O]) -> O:
        """Decode the outcome for ``ticket``. A ticket of another plan raises,
        and so does a rebuilt plan whose calls differ from the yielded plan's
        (the error names the first differing call)."""
        ...
    def plan(self) -> Effects: ...
    def outcome(self, index: int) -> Any:
        """Deprecated: positional access. Use ``ctx.get(ticket)`` with a ticket
        from ``plan.push(effect)``."""
        ...
    def complete(self, output: Any = ...) -> Any: ...

class HostOperationError(Exception):
    """A failed host operation in an async export; ``code`` is the ABI error code.
    An export that lets it escape fails the invocation with the same code."""
    code: str
    retryable: bool
    def __init__(self, code: str, message: str, retryable: bool = ...) -> None: ...

class AsyncContext(Protocol):
    """The context of an ``async def`` export (ABI 1.1 rounds, replay)."""
    input: Any
    def call(self, effect: TypedEffect[O]) -> Awaitable[O]:
        """A host operation; it registers when awaited or scheduled by
        ``gather``. Operations started together form one round."""
        ...
    def round(self) -> int:
        """The current invoke number; it must not influence which operations are called."""
        ...
    def max_rounds(self) -> int: ...
    def secret(self, alias: str) -> SecretHandle: ...
    def snapshot(self, resource: str) -> Snapshot: ...
    def patch(self, resource: str) -> Patch: ...
    def complete(
        self,
        output: Any = ...,
        *,
        patches: list[Any] = ...,
        transactions: list[Any] = ...,
        emitted_events: list[Any] = ...,
    ) -> Any: ...

def gather(*awaitables: Awaitable[Any], return_exceptions: bool = ...) -> Awaitable[list[Any]]:
    """Await operations or coroutines together; their host operations form
    one round group (``asyncio.gather`` semantics; no ``asyncio`` in the AOT profile)."""
    ...

class _Extension:
    def export(self, function: T) -> T: ...
    def resume(self, function: T) -> T: ...

extension: _Extension
