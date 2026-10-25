FROM python:3.12-alpine@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b
WORKDIR /opt/app
COPY server.py ./server.py
VOLUME /opt/app/server-data
USER 1000:1000
EXPOSE 8080
CMD ["python3", "server.py"]
