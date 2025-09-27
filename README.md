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
