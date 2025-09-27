# Build
```bash
docker build --tag radicle .
```
# Run
```bash
docker run \
  -u0 \
  -it \
  --rm \
  --mount type=bind,src=`pwd | sed -E 's/ /\\ /g'`,dst=/usr/src/app \
  --name radicle \
  radicle \
  /bin/bash
```

## analyzer
```bash
socat TCP-LISTEN:9257,reuseaddr EXEC:"docker exec -i radicle rust-analyzer"
```


