from flask import Flask

app = Flask(__name__)


@app.get("/health")
def health():
    return "pending", 503
