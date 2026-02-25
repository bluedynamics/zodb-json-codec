# Tutorials

<!-- diataxis: tutorials -->

Step-by-step lessons that guide you through using zodb-json-codec. Start with
the basics and work your way up to real ZODB record handling.

::::{grid} 2
:gutter: 3

:::{grid-item-card} Getting Started
:link: getting-started
:link-type: doc

Install the codec, convert your first pickle to JSON, and verify roundtrip
fidelity. Learn the marker format used for tuples, bytes, datetimes, and more.

*Estimated time: 10 minutes*
:::

:::{grid-item-card} Working with ZODB Records
:link: zodb-records
:link-type: doc

Decode and encode ZODB's two-pickle record format. Explore persistent
references, BTree flattening, and the single-pass PostgreSQL API.

*Estimated time: 15 minutes*
:::

::::

```{toctree}
---
hidden: true
---
getting-started
zodb-records
```
