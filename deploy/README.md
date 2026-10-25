# Deploying migrations

OpenMig deploys with a plain container: an image that contains the `openmig` binary and the
migration directory, run as a Kubernetes Job (or any one-shot task runner). No operator, no CRDs,
no ConfigMaps holding SQL files.

## Building the image

* **Nix**: `openmig.lib.mkMigrationsImage { inherit pkgs; name; tag; engine; migrations; }` produces
  an OCI image (`dockerTools.buildLayeredImage`). The migrations directory is copied to
  `/migrations`; the default command is
  `openmig deploy --engine <engine> --migrations /migrations --wait-for-db 2m`.
* **Docker**: [`docker/Dockerfile`](docker/Dockerfile) builds the same image from the OpenMig source
  tree with the migrations as a named build context.

Tag the image with the commit of the application repository: every schema change then produces a new
image and a new Job.

## Running it

* The database URL comes from `OPENMIG_URL` (typically a Secret). Logs are JSON
  (`OPENMIG_LOG_FORMAT=json`).
* [`kubernetes/job.yaml`](kubernetes/job.yaml): an Argo CD `Sync` hook in sync wave `-1`, so the
  migrations run before the application's Deployments are updated. Re-running is safe: with nothing
  pending, `deploy` exits within a second.
* [`helm/openmig-migrations`](helm/openmig-migrations): the same Job as a chart, as an Argo CD or Helm
  hook.
* [`kubernetes/cronjob-drift.yaml`](kubernetes/cronjob-drift.yaml): optional periodic
  `openmig status --fail-on-pending` (or `--drift` with an `openmig-full` image) for alerting.

`deploy` is safe to run concurrently (a PostgreSQL advisory lock or a ClickHouse Keeper lock),
retries transactional files that time out on locks, resumes non-transactional files after a fix, and
refuses checksum mismatches and out-of-order files. The first deploy into a database that already has
the schema uses `--baseline <version>`.

## Why not an operator

An operator would add continuous reconciliation and status in a custom resource. For schema
migrations that is not worth its cost:

* the migrations still have to reach the cluster, so an image (or a volume) is needed anyway;
* GitOps tools already provide ordering (sync waves / hooks), retries and visibility of Jobs;
* a controller brings CRD versioning, RBAC, an extra release to operate and a harder debugging story;
* "apply what is pending, then exit" is exactly a Job's lifecycle.

Revisit this if a fleet of databases (e.g. one per tenant) must be migrated from one place, or if
migrations must gate workloads in clusters without a GitOps tool.
