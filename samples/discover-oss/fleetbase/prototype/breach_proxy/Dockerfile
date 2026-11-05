FROM python@sha256:7e9b0661d175b176a592d3969ccaf702c11b6769a485fa8b0d3769dd07bf1720
COPY breach_proxy.py /opt/breach_proxy.py
USER 65532:65532
ENV PYTHONDONTWRITEBYTECODE=1
ENTRYPOINT ["python3", "/opt/breach_proxy.py"]
