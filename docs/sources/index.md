# zodb-json-codec

<!-- diataxis: landing -->

Fast pickle-to-JSON transcoder for ZODB, implemented in Rust via PyO3.

Converts ZODB pickle records into human-readable, JSONB-queryable JSON while maintaining full roundtrip fidelity.
Designed as the codec layer for a PostgreSQL JSONB storage backend.

**Key capabilities:**

- Full roundtrip fidelity: encode to JSON and back produces identical pickle bytes
- Human-readable JSON with compact type markers (`@dt`, `@ref`, `@kv`, ...)
- JSONB-queryable output for PostgreSQL
- Faster than CPython's C pickle extension on most operations
- GIL released during Rust phases for multi-threaded Python
- Direct JSON string path for zero-copy PostgreSQL storage
- BTree flattening for all BTrees package types
- Escape hatch (`@pkl`) ensures any pickle data roundtrips safely

**Requirements:** Python 3.10+, Rust toolchain (for building from source)

## Documentation

::::{grid} 2
:gutter: 3

:::{grid-item-card} Tutorials
:link: tutorials/index
:link-type: doc

**Learning-oriented** -- Step-by-step lessons to build skills.

*Start here if you are new to zodb-json-codec.*
:::

:::{grid-item-card} How-To Guides
:link: how-to/index
:link-type: doc

**Goal-oriented** -- Solutions to specific problems.

*Use these when you need to accomplish something.*
:::

:::{grid-item-card} Reference
:link: reference/index
:link-type: doc

**Information-oriented** -- Technical specifications and API details.

*Consult when you need detailed information.*
:::

:::{grid-item-card} Explanation
:link: explanation/index
:link-type: doc

**Understanding-oriented** -- Architecture, design decisions, and optimization history.

*Read to deepen your understanding of how it works.*
:::

::::

## Quick Start

1. {doc}`Install zodb-json-codec <how-to/install>`
2. {doc}`Encode and decode your first pickle <tutorials/getting-started>`
3. {doc}`Work with ZODB records <tutorials/zodb-records>`

```{toctree}
---
maxdepth: 3
caption: Documentation
titlesonly: true
hidden: true
---
tutorials/index
how-to/index
reference/index
explanation/index
```
