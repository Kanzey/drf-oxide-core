from collections.abc import Callable, Iterable
from typing import Any

__version__: str
STATUS_OK: int
STATUS_SKIP: int
STATUS_ERROR: int

class JsonFallback(Exception): ...

class CompiledSerializer:
    def __init__(self, read_fields: list[dict[str, Any]], write_fields: list[dict[str, Any]]) -> None: ...
    def to_representation(self, instance: Any, current_tz: Any = None) -> dict[str, Any]: ...
    def to_representation_many(self, iterable: Iterable[Any], current_tz: Any = None) -> list[dict[str, Any]]: ...
    def to_internal_value(
        self, data: dict[str, Any], current_tz: Any = None
    ) -> tuple[dict[str, Any], dict[str, Any] | None]: ...
    def describe(self) -> dict[str, dict[str, tuple[str, Any]]]: ...

def configure(
    *,
    empty: Any,
    skip_field: type[Exception],
    object_does_not_exist: type[Exception],
    pk_only_object: type,
    manager_class: type,
    dict_factory: Callable[[], dict[str, Any]],
    resolve_callable: Callable[[Any, str], Any],
    run_field: Callable[[Any, dict[str, Any], Callable[[Any], Any] | None], tuple[int, Any]],
    finish_field: Callable[[Any, Any, Callable[[Any], Any] | None, bool], tuple[int, Any]],
) -> None: ...
def is_configured() -> bool: ...
def to_json(
    obj: Any,
    *,
    ensure_ascii: bool = True,
    compact: bool = True,
    allow_nan: bool = False,
    default: Callable[[Any], Any] | None = None,
) -> bytes: ...
def from_json(data: bytes | bytearray | str, *, allow_nan: bool = False) -> Any: ...
