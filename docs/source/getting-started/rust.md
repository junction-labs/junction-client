# Installing Client - Rust

The core of Junction is written in Rust and is available in the
[`junction-core`](https://github.com/junction-labs/junction-client/tree/main/crates/junction-core)
crate. At the moment, we don't have an integration with an HTTP library
available, but you can use the core client to dynamically fetch config and
resolve addresses.

See the `examples` directory for [an
example](https://github.com/junction-labs/junction-client/blob/main/crates/junction-core/examples/get-endpoints.rs)
of how to use junction to resolve an address.

For more, [see the full API reference](https://docs.rs/junction-core/latest/junction_core/).
