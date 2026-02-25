# Reference

<!-- diataxis: reference -->

Technical specifications, API details, and format documentation for
zodb-json-codec.

::::{grid} 2
:gutter: 3

:::{grid-item-card} Python API
:link: python-api
:link-type: doc

Complete reference for all public functions exported by the
`zodb_json_codec` package. Includes signatures, parameters, return
types, and usage notes.
:::

:::{grid-item-card} JSON Format
:link: json-format
:link-type: doc

Full type mapping between Python/pickle types and their JSON
representations. Covers native types, structural markers, known type
markers, and fallback handling.
:::

:::{grid-item-card} BTree Format
:link: btree-format
:link-type: doc

JSON format for BTrees package types. Explains the `@kv`, `@ks`,
`@children`, `@first`, and `@next` markers used to flatten deeply
nested BTree state into queryable JSON.
:::

:::{grid-item-card} Project Structure
:link: project-structure
:link-type: doc

Rust source layout and module roles. Describes the data flow between
the decoder, encoder, JSON converter, and Python bridge.
:::

:::{grid-item-card} Changelog
:link: changelog
:link-type: doc

Version history with release notes, performance numbers, and
security fixes.
:::

::::

```{toctree}
---
hidden: true
---
python-api
json-format
btree-format
project-structure
changelog
```
