# douglas
A simple, opinionated container orchestrator that enables limited elastic infrastructure, but without high availability.  Elastic resources are limited to:
* Authentication
* Authorization
* Database
* Key Value Store
* Object Storage
* Secrets Vault
Each application can be hosted as a subdomain, or as the root application as needed.   Authentication between resources are automatically rotated, and user management is centralized.

It's intent is a quick-and-dirty self hosted solution where it's simple to mount containerized applications to a new subdomain, play around and remove as needed, and to easily test locally.

# Requirements
Runs on macOS or Linux.  Requires docker to be installed.

# Status
Watch this space for updates and details!

| Idx |                Milestone                | Status |                                                        Goal                                                       |
|-----|-----------------------------------------|-------|--------------------------------------------------------------------------------------------------------------------|
| M0  | Foundation                              |  ✅   | Base line proof of concept & learn rust: Service accounts, docker mounts, and reconciliation to to a working state |
| M1  | Host process supervision                |  ✅   | Add a watch dog to ensure that core processes and containers are running, restarting if needed                     |
| M2  | Douglas self-update                     |  ✅   | Implement a path to upgrade douglas system while running                                                           |
| M3  | External images + seedling environment  |  🟧   | Support hosting images from other repositories as seedlings, and add enhanced CLI tooling ergonomics               |
| M4  | TLS for every deployment                |  🔲   | Every deployment servces TLS, support local CA or external like Let's Encrypt                                      |
| M5  | Firewall + hardening                    |  🔲   | Make it safe to host!                                                                                              |
| M6  | Postgres core app                       |  🔲   | Introduce first "elastic" service                                                                                  |
| M7  | Ory stack                               |  🔲   | Add centralized user management, including OAuth2 and per-app opt-in authentication services                       |
| M8  | Blue-green deploy                       |  🔲   | Seedling deploys wiht minimal downtime                                                                             |
| M9  | Valkey + object storage                 |  🔲   | Add additional "elastic" services, KV via Valkey and an S3 like objevt storage system (tbd)                        |
| M10 | OpenBao Agent sidecar (finish)          |  🔲   | Internal mTLS, support automatic credential rotation for seedlings                                                 |
| M11 | Observability                           |  🔲   | Search, logs, graphs, dashboards, alerts (Prometheus, Loki, Grafana, Alertmanager)                                 |
| M12 | Security services                       |  🔲   | Autmated threat decection CAPTCHA services                                                                         |
| M13 | Remote management API                   |  🔲   | Expose dougals service commands via api for external management                                                    |
| M14 | Backup + restore                        |  🔲   | Full platform state can be rebuilt from one archive                                                                |
| M15 | Reference app (WriteFreely)             |  🔲   | Load a real application!                                                                                           |
| M16 | Core service memory footprint           |  🔲   | Do a pass on performance improvements, memory usage                                                                |
| M17 | Hardening + release                     |  🔲   | Ask the robots nicely to review the security capabilities of the system, then party                                |
