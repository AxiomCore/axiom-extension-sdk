# pyright strict type test of the async export surface (ABI 1.1 rounds).
from typing import Any

from axiom_extension_sdk import AsyncContext, HostOperationError, extension, gather, typed_effect

PRICE = typed_effect("catalog", "price", {"Unsigned": 1}, int)
NAME = typed_effect("catalog", "name", {"Unsigned": 1}, str)


@extension.export
async def apply(cx: AsyncContext, input: Any) -> Any:
    price: int = await cx.call(PRICE)
    name: str = await cx.call(NAME)
    both: list[Any] = await gather(cx.call(PRICE), cx.call(NAME))
    try:
        await cx.call(PRICE)
    except HostOperationError as error:
        code: str = error.code
        retryable: bool = error.retryable
        del code, retryable
    round_number: int = cx.round()
    return cx.complete({"price": price, "name": name, "both": both, "round": round_number}, patches=[cx.patch("ui:checkout").build()])


async def misuse(cx: AsyncContext) -> None:
    wrong: str = await cx.call(PRICE)  # type: ignore[assignment]
    del wrong
