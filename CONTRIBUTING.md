# Contributing to junction-client

Thanks for contributing to the junction-client repo!

## Code of Conduct

All Junction Labs repos adhere to the [Rust Code of Conduct][coc], without exception.

[coc]: https://www.rust-lang.org/policies/code-of-conduct

## Required Dependencies

This project depends on having a working `rust` toolchain, `protoc`, and `git`
installed locally to build and generate code.

Getting a working `rust` toolchain is beyond the scope of this project, but we
recommend checking out [rustup](https://rustup.rs/).

For getting started with `protoc`, see [the prost documentation][prost] and the
[official protoc documentation][protoc].

[prost]: https://docs.rs/prost-build/latest/prost_build/#sourcing-protoc
[protoc]: https://grpc.io/docs/protoc-installation/

## Updating Envoy Protobufs

This project uses a pinned list of protobuf dependency versions and their git
repositories to pull in protobuf defintions. Those versions are kept pinned in
`protobufs.toml`, and pulled from their git repositories at build time.

The pinned versions in `protobufs.toml` are kept in sync with a specific Envoy
commit. To update protobuf definitions, find a new Envoy revision to pin this
project to, and get the list of downstream dependency versions from the Envoy
project's bazel build rules. There is no easy way to do this, we're sorry.