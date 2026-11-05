"""Browser adapter to the pinned OpenVal engine and workbook functions."""

import json

from _lib import build_cashflow_report
from openval import Property
from openval.io import read_property_workbook, write_property_workbook


def cashflow(payload_json: str, frequency: str) -> str:
    result = build_cashflow_report(json.loads(payload_json), frequency=frequency)
    return json.dumps(result, allow_nan=False)


def parse_workbook(path: str) -> str:
    return read_property_workbook(path).model_dump_json()


def export_workbook(payload_json: str, path: str) -> None:
    prop = Property.model_validate(json.loads(payload_json))
    write_property_workbook(prop, path)
