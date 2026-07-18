FROM rust:1.97.1-bookworm@sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa AS build
WORKDIR /source
COPY . .
RUN cargo build --locked --release -p git-cdc-server
RUN mkdir -p /staging && chown 65532:65532 /staging

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:66aa873a4a14fb164aa01296058efd8253744606d72715e45acface073359faa
COPY --from=build /source/target/release/git-cdc-server /usr/local/bin/git-cdc-server
COPY --from=build /source/target/release/git-cdc-admin /usr/local/bin/git-cdc-admin
COPY --from=build --chown=65532:65532 /staging /var/lib/git-cdc/staging
EXPOSE 8080
ENV GIT_CDC_BIND=0.0.0.0:8080
ENV GIT_CDC_STAGING_DIR=/var/lib/git-cdc/staging
VOLUME ["/var/lib/git-cdc/staging"]
HEALTHCHECK --interval=15s --timeout=5s --retries=4 CMD ["/usr/local/bin/git-cdc-admin", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/git-cdc-server"]
