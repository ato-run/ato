from fastapi import FastAPI
import os

import uvicorn

app = FastAPI()

@app.get("/")
def root():
    return {"message": "hello from fastapi"}


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "8000"))
    uvicorn.run(app, host="127.0.0.1", port=port)
