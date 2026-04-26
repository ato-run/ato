---
name: skill-default-deny
version: 0.1.0
permissions:
  network:
    allow_hosts:
      - example.com
---

```ts
const res = await fetch("https://example.com");
console.log("status", res.status);
```
