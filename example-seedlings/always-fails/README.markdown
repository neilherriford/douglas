# Description
Same buffer program as `hello-world`, but with a health check that can never
pass (`command = "false"`). Exists to exercise the watchdog's give-up path:
a seedling whose container comes up fine but whose health check fails every
time, so the failure count climbs until `reached_max_fail_count` trips.

No Dockerfile of its own — reuses `hello-world`'s already-built image, same
as `second-app` does in the smoke tests.

## Registering the seedling
```bash
~/douglas seedling new --name always-fails --file default.toml
```

`route = "subdomain"`, served at `http://always-fails.localhost/` — `hello-world`
already holds the root route.

## Deploying
```bash
docker tag hello-world localhost:7376/always-fails
docker push localhost:7376/always-fails
```
