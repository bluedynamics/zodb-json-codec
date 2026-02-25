# Explanation

<!-- diataxis: explanation -->

Background knowledge, design decisions, and optimization history. These pages
help you understand *why* the codec works the way it does.

::::{grid} 2
:gutter: 3

:::{grid-item-card} Why Convert Pickle to JSON?
:link: why-json
:link-type: doc

The motivation behind the codec: what ZODB pickle gives you, what it
costs you, and why JSONB queryability changes the equation.
:::

:::{grid-item-card} Architecture
:link: architecture
:link-type: doc

Internal structure of the codec. Decode and encode pipelines, the
PickleValue AST, known-type interception, and the three decode output
paths.
:::

:::{grid-item-card} Performance
:link: performance
:link-type: doc

Current benchmark results with context. Synthetic micro-benchmarks,
FileStorage scans, PG storage path comparisons, and output size data.
:::

:::{grid-item-card} Optimization Journal
:link: optimization-journal
:link-type: doc

Chronological record of every performance optimization from v1.0.0
through v1.5.0. Techniques, insights, measured impact, and lessons
learned.
:::

:::{grid-item-card} Security
:link: security
:link-type: doc

Defense-in-depth measures against malformed pickle data. Length
validation, memo caps, recursion limits, and allocation guards.
:::

::::

```{toctree}
---
hidden: true
---
why-json
architecture
performance
optimization-journal
security
```
