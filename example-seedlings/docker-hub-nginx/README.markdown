# Description
Not a douglas-built image — this seedling references
`docker.io/nginxinc/nginx-unprivileged:1.27` directly via
`image.type = "external"`. The unprivileged variant is required because
douglas runs every seedling's container as its own non-root service account;
stock `nginx` tries to write to root-owned paths (`/var/cache/nginx/...`) on
startup and exits immediately under that model, while nginx-unprivileged
listens on 8080 and needs no root-owned paths. There's no Dockerfile and
nothing to build or push here: resin pulls the image itself, through its own
pull-through cache, the first time the seedling starts. See
`seedbank_types::ImageSource` for the manifest-level type this exercises.

## Registering the seedling
```bash
~/douglas seedling new --name docker-hub-nginx --file default.toml
```

Unlike a pushed seedling, nothing happens on `docker push` — there is none.
Reconcile (triggered by an explicit `seedling start`) is what pulls the
image, caches it under resin's `upstream/docker.io/...` tree, and starts the
container.

## Starting the seedling
```bash
~/douglas seedling start --name docker-hub-nginx
```

`health_check.command = "true"` deliberately does nothing beyond confirming
the container is up — nginx's own image has no `curl`/`wget` to check its
own port from inside, and proving the pull-through cache and routing work is
the point of this example, not nginx's health.
