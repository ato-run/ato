import os
os.chdir('/data/bench/sites')
import frappe
from synthetic_job import write_marker
frappe.init(site='single2.local', sites_path='/data/bench/sites')
frappe.connect()
frappe.enqueue(write_marker, queue='short', job_name='synthetic-proof', enqueue_after_commit=False)
frappe.destroy()
print('Synthetic job enqueued.')
