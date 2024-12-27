# Contributing to junction-client

Thanks for contributing to the junction-client repo!

## Code of Conduct

All Junction Labs repos adhere to the [Rust Code of Conduct][coc], without exception.

[coc]: https://www.rust-lang.org/policies/code-of-conduct

## Required Dependencies

This project depends on having a working `rust` toolchain. We currently do not have an
MSRV policy.

Working on `junction-python` or `junction-node` also requires having your own versions
of Python (>=3.8) or NodeJS installed.

## Building and Testing

This repo is managed as a Cargo workspace. Individual client bindings require their own
toolchains and may need to be managed on their own. For everyday development, we glue
things together with [cargo xtask](https://github.com/matklad/cargo-xtask) - if you need
to do something in development, it should be a standard `cargo` command or an `xtask`.
