import frappe

def write_marker():
    assert not frappe.flags.in_test, 'Worker unexpectedly inherited Frappe test mode'
    path = frappe.get_site_path('private', 'files', 'synthetic-queue-marker.txt')
    with open(path, 'w') as output:
        output.write('synthetic queue marker\n')
