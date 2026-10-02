# fast-drf-core

Rust core (PyO3 + maturin) of [`fast-drf`](../fast-drf). It knows nothing about Django; `fast_drf`
passes the DRF / Django objects it needs once through `configure()` and then builds a
`CompiledSerializer` per serializer instance from a plain-dict schema.

- `CompiledSerializer(read_fields, write_fields)`
  - `.to_representation(instance, current_tz)` / `.to_representation_many(iterable, current_tz)`
  - `.to_internal_value(data, current_tz)` -> `(validated, errors | None)`
  - `.describe()` - which strategy each field got (native kind or `python`)
- `to_json(obj, *, ensure_ascii, compact, allow_nan, default)` - byte-for-byte `json.dumps` with
  DRF's encoder; raises `JsonFallback` when it cannot guarantee that.
- `from_json(data, *, allow_nan)` - parsing via `jiter`.

Native code only handles the plain, valid case and returns "not handled" otherwise; the caller then
runs the DRF implementation, which produces the exact result or error.

## Development

```sh
uv venv && uv sync
make develop      # debug build into .venv; `make release` for benchmarks
make test         # cargo test + pytest
make lint
```

Requires Rust >= 1.83 (`jiter` 0.13 / `pyo3` 0.28).
