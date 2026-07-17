FROM rust:1.97.1-bookworm AS build
WORKDIR /source
COPY . .
RUN cargo build --locked --release -p git-cdc-server

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /source/target/release/git-cdc-server /usr/local/bin/git-cdc-server
COPY --from=build /source/target/release/git-cdc-admin /usr/local/bin/git-cdc-admin
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/git-cdc-server"]
