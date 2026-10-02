from collections import OrderedDict

import fast_drf_core


class Empty:
    pass


class SkipField(Exception):
    pass


class ObjectDoesNotExist(Exception):
    pass


class PKOnlyObject:
    def __init__(self, pk):
        self.pk = pk


class Manager:
    def __init__(self, items):
        self.items = items

    def all(self):
        return list(self.items)


def resolve_callable(value, attr):
    return value() if callable(value) and not isinstance(value, type) else value


def run_field(field, data, validate_method):
    return field.run(data.get(field.field_name, Empty), validate_method)


def finish_field(field, value, validate_method, run_validators):
    if validate_method is not None:
        value = validate_method(value)
    return fast_drf_core.STATUS_OK, value


if not fast_drf_core.is_configured():
    fast_drf_core.configure(
        empty=Empty,
        skip_field=SkipField,
        object_does_not_exist=ObjectDoesNotExist,
        pk_only_object=PKOnlyObject,
        manager_class=Manager,
        dict_factory=OrderedDict,
        resolve_callable=resolve_callable,
        run_field=run_field,
        finish_field=finish_field,
    )
