FROM python:3.12-alpine@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b
VOLUME /.ato-workspace/x
EXPOSE 8080
CMD ["python3", "-m", "http.server", "8080", "--directory", "/usr"]
