# Alpine runner image

On an x64 Linux host with Docker, build the ReleaseSafe binary with the shared
setup, then run from the repository root:

```sh
docker build --platform linux/amd64 -t oxap-alpine:verify .
NODE_ENV=test python3 .claude/skills/verify/scripts/alpine-image.py "$EVIDENCE/alpine"
```

Require exit 0 and `PASS: Alpine image completed one fake-endpoint turn;
requests=1`. The journey starts the image with isolated state and no provider
credentials. Its loopback endpoint returns a synthetic response. The image
contains the built-in loop only; pinned external harnesses are not bundled.
