# SiYuan Notes — startup URL is not the user URL

This sample exists to show a route whose `D.requirements.startup` differs from
the URL a user opens.

- The Contract observes `GET / -> 200` (the notes UI).
- The startup requirement polls `GET /api/system/bootProgress` and waits for
  `data.progress >= 100`.

The image is pinned to the upstream `b3log/siyuan` v3.8.9 linux/amd64 digest.
Nothing here has been validated on a real Linux Docker host in this change.
