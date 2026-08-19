# Description
Buffer program for testing deployments, auth, routing, etc. Serves whatever is
in the `public` mount as static files, with `index.html` as the default
document.

## Building
```bash
docker build . --tag hello-world
```

## Registering the seedling
```bash
~/douglas seedling new --name hello-world --file default.toml
```

Two spec files are provided: `default.toml` (`route = "root"`, served at
`http://localhost/`) and `subdomain.toml` (`route = "subdomain"`, served at
`http://<name>.localhost/`) — only one seedling can hold the root route at a
time, so a second seedling needs `subdomain.toml`. Each declares the `public`
mount, its starter `index.html` (inlined under `mounts.public.contents`, with
copy naming which file it came from so the two are distinguishable when both
are running), and the port douglas/traefik should route to (3000, matching
`EXPOSE`/`server.js`). Reconcile creates the mount folder and writes that
file to it on first reconcile — no manual copy step needed.

## Deploying
```bash
docker tag hello-world localhost:7376/<name>
docker push localhost:7376/<name>
```

The push triggers bract to reconcile the seedling: it builds/starts the
container, joins it to its per-seedling network, and writes/attaches the
traefik route.
