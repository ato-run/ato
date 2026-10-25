FROM python:3.12-alpine
RUN mkdir -p /work/.tmp && TMPDIR=/work/.tmp pip install --no-cache-dir openai==3.37.0
ENV PYTHONDONTWRITEBYTECODE=1 TMPDIR=/work/.tmp
WORKDIR /app
USER 65534:65534
ENTRYPOINT ["python", "/app/client.py"]
