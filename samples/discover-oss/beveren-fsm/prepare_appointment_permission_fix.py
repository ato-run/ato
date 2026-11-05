#!/usr/bin/env python3
"""Prepare narrow appointment read checks for the pinned upstream source."""

import argparse
import hashlib
from pathlib import Path

UPSTREAM_SHA256 = "29671e6c15b8ba026f3ed1524603a1f662c41f2843d7d8b85778203e531c76c8"


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError("Unexpected appointment source structure")
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream", type=Path)
    parser.add_argument("output", type=Path, help="new file; never overwritten")
    args = parser.parse_args()
    content = args.upstream.read_bytes()
    if hashlib.sha256(content).hexdigest() != UPSTREAM_SHA256:
        raise ValueError("Unexpected upstream appointment source")
    text = content.decode("utf-8")
    text = replace_once(text, 'appointments = frappe.get_all(\n', 'appointments = frappe.get_list(\n')
    text = replace_once(text,
        '\t\tappointment_doc = frappe.get_doc("Service Appointment", appointment.name)\n',
        '\t\tappointment_doc = frappe.get_doc("Service Appointment", appointment.name)\n'
        '\t\tappointment_doc.check_permission("read")\n')
    text = replace_once(text,
        '\tappointment_data = appointment_doc.as_dict(convert_dates_to_str=True)\n',
        '\tappointment_doc.check_permission("read")\n'
        '\tappointment_data = appointment_doc.as_dict(convert_dates_to_str=True)\n')
    # Linked location is supplementary: do not reveal it without access to both documents.
    for indent in ('\t\t\t\t', '\t\t\t'):
        order = '\n' + indent + 'service_order = frappe.get_doc("Service Order", appointment_data["service_order"])\n'
        text = replace_once(text, order, order + indent + 'service_order.check_permission("read")\n')
        area = '\n' + indent + '\t\tservice_area_doc = frappe.get_doc("Service Area", service_area_name)\n'
        text = replace_once(text, area, area + indent + '\t\tservice_area_doc.check_permission("read")\n')
    # If the linked area is inaccessible, the fallback must not disclose its name.
    text = text.replace('except Exception:\n\t\t\t\t\t\t# Silently skip if Service Area',
                        'except Exception:\n\t\t\t\t\t\tservice_area_name = None\n\t\t\t\t\t\t# Silently skip if Service Area', 1)
    text = text.replace('except Exception:\n\t\t\t\t\t# Silently skip if Service Area',
                        'except Exception:\n\t\t\t\t\tservice_area_name = None\n\t\t\t\t\t# Silently skip if Service Area', 1)
    start = text.index('def get_invoices_for_appointment(')
    section = text[start:]
    section = replace_once(section, '\tfields = [\n',
        '\tappointment = frappe.get_doc("Service Appointment", appointment_name)\n'
        '\tappointment.check_permission("read")\n'
        '\tif service_order:\n'
        '\t\tif service_order != appointment.service_order:\n'
        '\t\t\tfrappe.throw("Service Order does not match appointment", frappe.PermissionError)\n'
        '\t\tfrappe.get_doc("Service Order", service_order).check_permission("read")\n\n'
        '\tfields = [\n')
    section = replace_once(section, 'return frappe.get_all("Sales Invoice",',
                           'return frappe.get_list("Sales Invoice",')
    text = text[:start] + section
    compile(text, args.upstream.name, "exec")
    result = text.encode("utf-8")
    with args.output.open("xb") as output:
        output.write(result)
    print(f"Prepared appointment source sha256:{hashlib.sha256(result).hexdigest()}")


if __name__ == "__main__":
    main()
