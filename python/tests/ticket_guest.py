from typing import TYPE_CHECKING

from axiom_extension_sdk import extension

if TYPE_CHECKING:
    from axiom_extension_sdk import Context, Effects, ResumeContext, Ticket


def lookup(fx: "Effects", item: int) -> "tuple[Ticket[str], Ticket[bool]]":
    name: "Ticket[str]" = fx.push({"namespace": "catalog", "operation": "name", "input": {"Unsigned": item}})
    stock: "Ticket[bool]" = fx.push({"namespace": "catalog", "operation": "stock", "input": {"Unsigned": item}})
    return name, stock


@extension.export
def run(ctx: "Context"):
    fx = ctx.plan()
    lookup(fx, ctx.input)
    return ctx.effects(fx)


@extension.export
def raw(ctx: "Context"):
    item = ctx.input
    return ctx.effects(
        {"namespace": "catalog", "operation": "name", "input": {"Unsigned": item}},
        {"namespace": "catalog", "operation": "stock", "input": {"Unsigned": item}},
    )


@extension.resume
def resume(ctx: "ResumeContext"):
    # The rebuild repeats the yielded calls exactly, inputs included (the
    # test invokes `run` with 7): `get` checks it against the yielded plan.
    fx = ctx.plan()
    name, stock = lookup(fx, 7)
    result = {"name": ctx.get(name), "stock": ctx.get(stock), "legacy": ctx.outcome(0), "index": stock.index}
    try:
        other = ctx.plan()
        foreign, _ = lookup(other, 7)
        ctx.get(foreign)
        result["mixed"] = "accepted"
    except Exception as error:
        result["mixed"] = str(error)
    return ctx.complete(result)
