"""Official SDK inside an isolated test container, with no provider credential."""
import json
import os
import socket
from openai import OpenAI, APIStatusError
client = OpenAI(timeout=3, max_retries=0)
for encoding in (None, "float"):
    options = {} if encoding is None else {"encoding_format": encoding}
    result = client.embeddings.create(model="text-embedding-3-small", input="one bounded document", **options)
    assert len(result.data[0].embedding) == 1536
    assert result.data[0].embedding[0] == 0.25
    assert "sk-sdk-upstream-canary" not in result.model_dump_json()
try:
    client.chat.completions.create(model="chat-model", messages=[{"role":"user","content":"forbidden"}])
except APIStatusError as error:
    assert error.status_code == 400
else:
    raise AssertionError("relay allowed chat")
for host, port in [("1.1.1.1", 443), (os.environ["RELAY_GATEWAY"], 22), (os.environ["RELAY_GATEWAY"], 443)]:
    try:
        socket.create_connection((host, port), timeout=1).close()
    except OSError:
        continue
    raise AssertionError("direct provider or host access was permitted")
print(json.dumps({"sdk":"openai-3.37.0", "embedding_requests":2,"dimensions":1536,"refused_chat":True,"direct_network_denials":3}))
