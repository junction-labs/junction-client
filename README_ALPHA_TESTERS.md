# Hello Alpha Testers

Thanks for helping us test Junction!

Before you get started on this doc, make sure you read the
[README](https://github.com/junction-labs/junction-client#readme). It contains a
high-level overview of the project and some basic installation instructions.
Once you're done, come back here.

## Rapid Changes and Feedback

We're developing in the open and iterating pretty rapidly for the next couple of
months. Expect things to change pretty quickly around here.

We're looking for as much feedback as we can get at this stage. During these
early stages, please reach out to us directly or in the Discord rather than
opening GitHub issues. We expect most of the early feedback to be design
related, and we'd prefer to work through that kind of feedback interactively
with you.

## Our Focus Right Now (10/3/2024)

Right now, we're focused on getting the client side experience right and
figuring out the high-level bits of the library. As we're iterating on UX with
alpha testers, we're going to complete the loop and build APIs for saving your
config back to a central service and dynamically pushing it to clients.

## Known Bugs/Limitations

* The Junction client will only route to Kubernetes services in the cluster
  your control plane runs in. If you try something like
  `session.get("http://example.com")` it won't route.

## Installing from Source

We're still early enough that we don't want to push packaged versions of our code
to PyPi and crate.io yet. For the meantime, we're going to ask you to build and
install Junction from source.

### Pre-requisites

To get going on Junction, you need a working Rust toolchain and a system Python
that you can use to bootstrap a virtualenv.

If you don't have Rust installed, use [rustup](https://rustup.rs/) to get started.

### Using Junction in Python

To install `junction` into a virtualenv in this directory (`.venv`), run:

```shell
cargo xtask python-build
```

That's it! After you're done, run `source .venv/bin/activate` your virtualenv
and get started with `import junction`. Head back to the `README` and our
samples to get started.

To install `junction` into any other virtualenv, set your `VIRTUAL_ENV`
environment variable and run `cargo xtask python-build`

### Using Junction in Rust

If you'd like to try Junction in plain Rust, feel free. We're not focusing on
the experience here yet, but feedback is still welcome.

Take a dependency on the `junction_core` crate with by adding `junction_core = {
version = "0.1", path = "path/to/your/clone/crate/junction-core"}` to your
`Cargo.toml` and `use junction_core`. Check out the README for an example.
