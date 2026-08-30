# Description
Exercises OpenBao secrets access end to end, the same role `hello-world`
plays for traefik/routing. On every request it writes a timestamped greeting
to its own KV namespace through its OpenBao agent sidecar, reads it back,
and serves the round-tripped value as the response — so a successful `curl`
through traefik is proof the whole chain (agent auto-auth → KV write → KV
read) actually worked, not just that the container started.

This container never sees an OpenBao credential of its own — it just points
[`node-vault`](https://github.com/nodevault/node-vault), an ordinary
community Vault client library, at its agent sidecar's local address
(`OPENBAO_AGENT_ADDR`) with no token at all. The agent authenticates on its
own behalf and injects the token transparently, so the app's own filesystem
never holds a reusable OpenBao credential.

## Requirements
OpenBao must already be initialized, unsealed, and have douglas's own AppRole
credentials provisioned (`douglas status` should show `credentials work:
true`) before this seedling can reconcile — `bract` needs to log in as
douglas itself to mint this seedling's own scoped AppRole.

## Building
```bash
docker build . --tag secrets
```

Needs network access during the build to `npm ci` its one dependency
(`node-vault`, pinned via `package-lock.json`).

## Registering the seedling
```bash
~/douglas seedling new --name secrets --file default.toml
```

`default.toml` declares `secrets = { read = true, write = true }` and no
mounts of its own — bract mints an AppRole scoped to `kv/data/secrets/*` and
`kv/metadata/secrets/*` and stands up a second, sidecar container
(`doug-agent.secrets`) running OpenBao Agent to hold it. This container only
gets an `OPENBAO_AGENT_ADDR` environment variable pointing at that sidecar;
it never receives the AppRole credentials directly.

## Deploying
```bash
docker tag secrets localhost:7376/secrets
docker push localhost:7376/secrets
```

The push triggers bract to reconcile the seedling: mint (or reuse) its
OpenBao credentials, build/start the container, join it to its per-seedling
network, and write/attach the traefik route.

## Verifying
```bash
curl http://secrets.localhost/
```

Each request re-writes the greeting with a fresh timestamp and reads it
back — so two requests a moment apart should return two different
timestamps, confirming the write actually landed rather than being served
from a cache.
