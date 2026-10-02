import datetime
import decimal
import types

import pytest
from conftest import Manager, SkipField

from fast_drf_core import STATUS_ERROR, STATUS_OK, CompiledSerializer


class FakeField:
    def __init__(self, field_name='', repr_fn=str, error=None):
        self.field_name = field_name
        self.repr_fn = repr_fn
        self.error = error
        self.calls = 0

    def to_representation(self, value):
        self.calls += 1
        return self.repr_fn(value)

    def get_attribute(self, instance):
        raise SkipField()

    def run(self, raw, validate_method):
        if self.error is not None:
            return STATUS_ERROR, [self.error]
        return STATUS_OK, f'python:{raw}'


def read(name, repr_, get=None, field=None):
    return {
        'name': name,
        'field': field or FakeField(name),
        'get': get or {'type': 'attrs', 'attrs': [name]},
        'repr': repr_,
    }


def write(name, val, **extra):
    return {'name': name, 'field': extra.pop('field', FakeField(name)), 'source_attrs': [name], 'val': val, **extra}


def test_primitives_and_nested():
    child = CompiledSerializer([read('id', {'type': 'int'})], [])
    serializer = CompiledSerializer(
        [
            read('title', {'type': 'str'}),
            read('price', {'type': 'decimal', 'decimal_places': 2, 'max_digits': 5, 'coerce_to_string': True}),
            read('day', {'type': 'date'}),
            read('owner', {'type': 'nested', 'serializer': child}),
            read('items', {'type': 'nested', 'serializer': child, 'many': True}),
            read('missing', {'type': 'str'}),
        ],
        [],
    )
    obj = types.SimpleNamespace(
        title='Book',
        price=decimal.Decimal('12.50'),
        day=datetime.date(2024, 5, 1),
        owner=types.SimpleNamespace(id=7),
        items=Manager([types.SimpleNamespace(id=1), types.SimpleNamespace(id=2)]),
    )
    result = serializer.to_representation(obj)
    assert dict(result) == {
        'title': 'Book',
        'price': '12.50',
        'day': '2024-05-01',
        'owner': {'id': 7},
        'items': [{'id': 1}, {'id': 2}],
    }
    assert [dict(r) for r in serializer.to_representation_many([obj])] == [result]


def test_falls_back_to_field_for_unknown_types():
    field = FakeField('price', repr_fn=lambda v: 'fallback')
    serializer = CompiledSerializer(
        [read('price', {'type': 'decimal', 'decimal_places': 2, 'max_digits': 5}, field=field)], []
    )
    assert serializer.to_representation({'price': decimal.Decimal('1.234')})['price'] == 'fallback'
    assert field.calls == 1


def test_none_is_not_passed_to_field():
    serializer = CompiledSerializer([read('a', {'type': 'python'})], [])
    assert serializer.to_representation({'a': None}) == {'a': None}


def test_native_validation_and_fallback():
    serializer = CompiledSerializer(
        [],
        [
            write('name', {'type': 'char', 'max_length': 5}),
            write('count', {'type': 'int', 'min_value': 0}),
            write('price', {'type': 'decimal', 'max_digits': 5, 'decimal_places': 2}),
            write('flag', {'type': 'bool'}),
            write('tags', {'type': 'list', 'child': {'type': 'char'}}),
            write('bad', {'type': 'int'}, field=FakeField('bad', error='invalid')),
        ],
    )
    ret, errors = serializer.to_internal_value(
        {'name': '  abc ', 'count': '12', 'price': '1.5', 'flag': 'yes', 'tags': ['a', ' b'], 'bad': 'x'}
    )
    assert dict(ret) == {
        'name': 'abc',
        'count': 12,
        'price': decimal.Decimal('1.50'),
        'flag': True,
        'tags': ['a', 'b'],
    }
    assert dict(errors) == {'bad': ['invalid']}

    ret, errors = serializer.to_internal_value({'name': 'too long', 'count': -1})
    assert (ret['name'], ret['count']) == ('python:too long', 'python:-1')


@pytest.mark.parametrize('value', ['1.555', '1e3', 'abc', '123456'])
def test_decimal_edge_cases_fall_back(value):
    serializer = CompiledSerializer([], [write('d', {'type': 'decimal', 'max_digits': 5, 'decimal_places': 2})])
    ret, _ = serializer.to_internal_value({'d': value})
    assert ret['d'] == f'python:{value}'
