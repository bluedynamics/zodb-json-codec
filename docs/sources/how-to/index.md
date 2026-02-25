# How-To Guides

<!-- diataxis: how-to -->

Goal-oriented recipes for common tasks.
Each guide answers a specific "how do I..." question.

::::{grid} 2
:gutter: 3

:::{grid-item-card} Install zodb-json-codec
:link: install
:link-type: doc

Install from PyPI with pre-built wheels, or with uv.
Verify the installation works.
:::

:::{grid-item-card} Integrate with zodb-pgjsonb
:link: integrate-pgjsonb
:link-type: doc

Use the codec as the transcoding layer for PostgreSQL JSONB storage.
Understand the fast paths and the data pipeline.
:::

:::{grid-item-card} Handle custom and unknown types
:link: handle-custom-types
:link-type: doc

Learn how the codec handles types it does not recognize,
and what the `@reduce` and `@pkl` markers mean.
:::

:::{grid-item-card} Run benchmarks
:link: run-benchmarks
:link-type: doc

Run synthetic and real-world benchmarks.
Build with PGO for production-accurate numbers.
:::

:::{grid-item-card} Build from source
:link: build-from-source
:link-type: doc

Set up a development environment with Rust, maturin, and Python.
Run the Rust and Python test suites.
:::

::::

```{toctree}
---
hidden: true
---
install
integrate-pgjsonb
handle-custom-types
run-benchmarks
build-from-source
```
