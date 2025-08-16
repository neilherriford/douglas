# Error cases
## Malformed request
```bash
./post.sh /tmp/smelly.sock '{"Malformed": {"token": "ooops", "service_name": "zordon"}}'
```
## Invalid token
```bash
./post.sh /tmp/smelly.sock '{"type": "CreateCredentials", "payload": {"token": "❌", "service_name": "zordon"}}'
```

# Success case
## Create credentials
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "CreateCredentials", "payload": {"token": "'"$token"'", "service_name": "zordon"}}'
```

## Create mount
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "CreateMount", "payload": {"token": "'"$token"'", "service_name": "zordon", "mount_name": "hood"}}'
```

## List available mounts
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "ListMountVersions", "payload": {"token": "'"$token"'", "service_name": "zordon", "mount_name": "hood"}}'
```

## List active mount
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "ActiveMountVersion", "payload": {"token": "'"$token"'", "service_name": "zordon", "mount_name": "hood"}}'
```

## Create new mount version
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "CreateNewMountVersion", "payload": {"token": "'"$token"'", "service_name": "zordon", "mount_name": "hood"}}'
```

## Set mount version
```bash
token=$(</tmp/doug.token) ; ./post.sh /tmp/smelly.sock '{"type": "SetMountVersion", "payload": {"token": "'"$token"'", "service_name": "zordon", "mount_name": "hood", "version": 0}}'
```
