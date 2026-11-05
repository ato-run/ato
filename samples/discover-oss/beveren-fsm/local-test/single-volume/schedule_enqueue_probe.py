import os
os.chdir('/data/bench/sites')
import frappe
frappe.init(site='single2.local', sites_path='/data/bench/sites')
frappe.connect()
name=frappe.db.get_value('Scheduled Job Type', {'method':'frappe.ping'}, 'name')
if not name:
    doc=frappe.get_doc({'doctype':'Scheduled Job Type','method':'frappe.ping','frequency':'Cron','cron_format':'* * * * *'})
    doc.insert(ignore_permissions=True)
    name=doc.name
frappe.db.set_value('Scheduled Job Type', name, 'last_execution', '2020-01-01 00:00:00')
frappe.db.commit()
doc=frappe.get_doc('Scheduled Job Type',name)
assert doc.is_event_due() and doc.enqueue()
print('Due synthetic scheduled job enqueued:', name)
frappe.destroy()
