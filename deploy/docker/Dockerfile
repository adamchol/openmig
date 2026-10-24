# An image that applies the migrations in a directory to the database in $OPENMIG_URL.
# Build from the OpenMig source tree with the migrations as a named build context:
#
#   docker build -f deploy/docker/Dockerfile \
#     --build-context migrations=/path/to/app/db/migrations \
#     --build-arg ENGINE=postgres \
#     -t ghcr.io/acme/app-migrations:$(git -C /path/to/app rev-parse --short HEAD) .
FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p openmig

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/openmig /usr/local/bin/openmig
COPY --from=migrations . /migrations
ARG ENGINE=postgres
ENV OPENMIG_ENGINE=${ENGINE} OPENMIG_LOG_FORMAT=json
USER 65534:65534
ENTRYPOINT ["openmig"]
CMD ["deploy", "--migrations", "/migrations", "--wait-for-db", "2m"]
