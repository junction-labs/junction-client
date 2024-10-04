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

Right now, we're focused on getting the UX and data model for Routes and
Backends correct, and to a place where people feel like they can do something
useful with them. That means right now they're front and center in every client.

Long term, we absolutely don't envision most of the usage of Junction to involve
putting routes in your client - we want as much config as possible to be pushed
dynamically over the network, and mostly invisible. The next thing we're
focusing on is the workflow for saving Routes and Backends to a central config
store and serving them dynamically. Stay tuned for that.

## Known Bugs/Limitations

* The Junction client will only route to Kubernetes services in the cluster
  your control plane runs in. If you try something like
  `session.get("http://example.com")` it won't route.

* We're not automatically generating Python API documentation yet - we're
betting on our examples and docstrings for now. The config API is fully
represented and documented in [`config.py`][config-py] and you should get
auto-complete and pop-up documentation in any editor that supports it.

* We currently fail `mypy` type checking. We generate our config types in such
a way that doesn't expose which fields are optional and which are required. We're
still figuring out if we want to do dict-style config before we spend more time
on making this better, so if you're someone who regularly uses `mypy` or have an
opinion on dicts vs. classes, please reach out!

[config-py]: https://github.com/junction-labs/junction-client/blob/main/junction-python/junction/config.py

## Getting set up

We're still early enough that we don't want to push packaged versions of our code
to PyPi and crates.io yet. For the meantime, we're going to ask you to build and
install Junction and a simple control plane from source.

### Rust and Python

To get going on Junction, you need a working Rust toolchain and a system Python
that you can use to bootstrap a virtualenv. If you don't have Rust installed,
use [rustup](https://rustup.rs/) to get started.

### Kubernetes

To do anything interesting with Junction, you currently need a running
Kubernetes cluster. If you don't have strong opinions about how to set up your
own cluster, we recommend using the built-in cluster in OrbStack or Docker
Desktop.

### `ezbake`

Once you've gotten both Rust and a Kubernetes cluster running, you need a
running control plane. Install our `ezbake` control plane by following the
instructions in [its README][ezbake-readme].

[ezbake-readme]: https://github.com/junction-labs/ezbake#readme

## Using Junction in Python

To install `junction` into a virtualenv in this directory (`.venv`), run:

```shell
cargo xtask python-build
```

That's it! After you're done, run `source .venv/bin/activate` to activate your
virtualenv and get started with `import junction`. Head back to the `README` and
our samples and see what you can cook up.

> **ADVANCED TIP**: To install `junction` into any other virtualenv, set your
`VIRTUAL_ENV` environment variable and run `cargo xtask python-build`

### Using Junction in Rust

If you'd like to try Junction in plain Rust, feel free. We're not focusing on
the experience yet, but would still welcome feedback on the core APIs.

Take a dependency on the `junction_core` crate with by adding `junction_core = {
version = "0.1", path = "path/to/your/clone/crate/junction-core"}` to your
`Cargo.toml` and `use junction_core`. Check out the README for an example.
