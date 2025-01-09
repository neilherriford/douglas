# Description
Buffer program for testing deployments, auth, etc.

## Building
```bash
docker build . --tag hello-world
```

## Running
```bash
docker run \
--rm \
--tty \
--interactive \
--env PORT=3000 \
--env RESPONSE_MESSAGE='hello, world!' \
--publish 3000:3000 \
--name hello-world \
hello-world
```
