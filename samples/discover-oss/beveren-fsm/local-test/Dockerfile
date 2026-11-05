FROM frappe/erpnext@sha256:6e016c0fc3aec5b32c14ff7c2ac97523cc864748c83ce75197779b8fe5a665a8
USER frappe
COPY --chown=frappe:frappe app /home/frappe/frappe-bench/apps/beveren_fsm
COPY --chown=frappe:frappe flit_core-3.12.0-py3-none-any.whl /home/frappe/frappe-bench/.ato-build/flit_core-3.12.0-py3-none-any.whl
RUN mkdir -p /home/frappe/frappe-bench/.ato-build/tmp && \
    TMPDIR=/home/frappe/frappe-bench/.ato-build/tmp env/bin/pip install --no-index --no-deps --no-cache-dir /home/frappe/frappe-bench/.ato-build/flit_core-3.12.0-py3-none-any.whl && \
    TMPDIR=/home/frappe/frappe-bench/.ato-build/tmp env/bin/pip install --no-index --no-deps --no-cache-dir --no-build-isolation -e apps/beveren_fsm
USER root
RUN ln -s /home/frappe/frappe-bench/apps/beveren_fsm/beveren_fsm/public /home/frappe/frappe-bench/assets/beveren_fsm
USER frappe
