"""Static type test for the ticket API (checked by pyright in strict mode with
reportUnnecessaryTypeIgnoreComment, so every ignore below must be needed)."""
from typing import Any

from axiom_extension_sdk import Context, ResumeContext, Ticket, TypedEffect


def build(ctx: Context) -> tuple[Ticket[str], Ticket[bool], Ticket[Any]]:
    plan = ctx.plan()
    name_effect: TypedEffect[str] = {"namespace": "catalog", "operation": "name", "input": {"Unsigned": 1}}
    stock_effect: TypedEffect[bool] = {"namespace": "catalog", "operation": "stock", "input": {"Unsigned": 1}}
    name = plan.push(name_effect)
    stock = plan.push(stock_effect)
    # An untyped effect (any mapping that is not a TypedEffect) gives Ticket[Any].
    raw_effect: dict[str, Any] = {"namespace": "catalog", "operation": "raw", "input": "Null"}
    raw = plan.push(raw_effect)
    ctx.effects(plan)
    return name, stock, raw


def read(ctx: ResumeContext, name: Ticket[str], stock: Ticket[bool]) -> str:
    text: str = ctx.get(name)
    available: bool = ctx.get(stock)
    index: int = name.index
    wrong: int = ctx.get(name)  # pyright: ignore[reportAssignmentType]
    swapped: Ticket[str] = stock  # pyright: ignore[reportAssignmentType]
    ctx.get(0)  # pyright: ignore[reportArgumentType]
    del wrong, swapped
    return f"{text}{available}{index}"
