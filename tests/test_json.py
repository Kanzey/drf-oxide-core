import datetime
import decimal
import json
import math
import uuid
from collections import OrderedDict

import pytest

from fast_drf_core import JsonFallback, from_json, to_json


def drf_like_default(obj):
    if isinstance(obj, datetime.datetime):
        representation = obj.isoformat()
        if representation.endswith('+00:00'):
            representation = representation[:-6] + 'Z'
        return representation
    if isinstance(obj, datetime.date):
        return obj.isoformat()
    if isinstance(obj, decimal.Decimal):
        return float(obj)
    if isinstance(obj, uuid.UUID):
        return str(obj)
    if isinstance(obj, set):
        return tuple(obj)
    raise TypeError(f'Object of type {type(obj).__name__} is not JSON serializable')


def reference(obj, *, ensure_ascii=True, compact=True, allow_nan=False):
    separators = (',', ':') if compact else (', ', ': ')
    ret = json.dumps(obj, default=drf_like_default, ensure_ascii=ensure_ascii, allow_nan=allow_nan, separators=separators)
    return ret.replace('\u2028', '\\u2028').replace('\u2029', '\\u2029').encode()


class IntEnumLike(int):
    pass


VALUES = [
    None,
    True,
    False,
    0,
    -1,
    2**63,
    -(2**70),
    IntEnumLike(5),
    0.0,
    -0.0,
    1.5,
    1e16,
    1e-7,
    123456789.123456789,
    '',
    'zażółć gęślą jaźń',
    'quote " backslash \\ newline \n tab \t \x00 \x1f \x7f',
    '\u2028\u2029',
    '😀 emoji',
    [],
    (),
    [1, [2, [3, []]]],
    {},
    {'a': 1, 'b': [None, True]},
    {1: 'int key', 1.5: 'float key', True: 'bool key', None: 'none key'},
    OrderedDict([('z', 1), ('a', 2)]),
    datetime.datetime(2024, 1, 2, 3, 4, 5, 6),
    datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc),
    datetime.date(2024, 1, 2),
    decimal.Decimal('1.10'),
    uuid.UUID('12345678-1234-5678-1234-567812345678'),
    {'nested': {'set': {1}}},
]


@pytest.mark.parametrize('value', VALUES, ids=repr)
@pytest.mark.parametrize('ensure_ascii', [True, False])
@pytest.mark.parametrize('compact', [True, False])
def test_to_json_matches_json_dumps(value, ensure_ascii, compact):
    expected = reference(value, ensure_ascii=ensure_ascii, compact=compact)
    actual = to_json(value, ensure_ascii=ensure_ascii, compact=compact, default=drf_like_default)
    assert actual == expected


def test_ordered_dict_after_move_to_end():
    value = OrderedDict([('a', 1), ('b', 2)])
    value.move_to_end('a')
    assert to_json(value) == reference(value)


@pytest.mark.parametrize('value', [math.nan, math.inf, -math.inf])
def test_non_finite_floats(value):
    with pytest.raises(ValueError):
        to_json(value)
    assert to_json(value, allow_nan=True) == reference(value, allow_nan=True)


def test_unknown_type_without_default():
    with pytest.raises(TypeError):
        to_json(object())


def test_bad_key():
    with pytest.raises(TypeError):
        to_json({(1, 2): 1})


def test_lone_surrogate_falls_back():
    with pytest.raises(JsonFallback):
        to_json('\ud800')


@pytest.mark.parametrize(
    'text',
    [
        '{}',
        '[]',
        '{"a": [1, 2.5, -3e2, "x", null, true, false, {"b": {}}]}',
        '"\\u017c\\ud83d\\ude00"',
        '123456789012345678901234567890',
        '  {"dup": 1, "dup": 2}  ',
    ],
)
def test_from_json_matches_json_loads(text):
    assert from_json(text.encode()) == json.loads(text)
    assert from_json(text) == json.loads(text)


def test_from_json_rejects_invalid():
    with pytest.raises(ValueError):
        from_json(b'{"a": }')


def test_from_json_nan():
    with pytest.raises(ValueError):
        from_json(b'[NaN]')
    assert math.isnan(from_json(b'[NaN]', allow_nan=True)[0])
