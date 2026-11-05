import os
os.chdir('/data/bench/sites')
import frappe
frappe.init(site='single2.local', sites_path='/data/bench/sites')
frappe.connect()
name=frappe.db.get_value('Scheduled Job Type', {'method':'frappe.ping'}, 'name')
rows=frappe.get_all('Scheduled Job Log', filters={'scheduled_job_type':name}, fields=['status'], order_by='creation desc', limit=1)
assert rows and rows[0].status=='Complete', rows
print('Synthetic scheduled job completed.')
frappe.destroy()
