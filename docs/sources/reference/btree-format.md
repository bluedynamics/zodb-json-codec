# BTree Format

<!-- diataxis: reference -->

BTrees from the `BTrees` package store their state as deeply nested
tuples in pickle. The codec flattens these into human-readable,
JSONB-queryable JSON while preserving full roundtrip fidelity.

## Supported BTree Classes

All classes matching the pattern `BTrees.{PREFIX}BTree.{PREFIX}{Type}`
are recognized, where `{Type}` is one of `BTree`, `Bucket`, `TreeSet`,
or `Set`.

### Prefixes

| Prefix | Key Type | Value Type |
|---|---|---|
| `OO` | object | object |
| `IO` | integer | object |
| `OI` | object | integer |
| `II` | integer | integer |
| `LO` | long | object |
| `OL` | object | long |
| `LL` | long | long |
| `LF` | long | float |
| `IF` | integer | float |
| `QQ` | unsigned long | unsigned long |
| `fs` | FileStorage | (internal) |

### Node Types

BTree
: Tree node for maps. Small trees use 4-level tuple nesting for inline
  data; large trees use persistent references to child buckets.

Bucket
: Leaf node for maps. Uses 2-level tuple nesting for inline key-value
  data.

TreeSet
: Tree node for sets. Same structure as BTree but stores keys only.

Set
: Leaf node for sets. Same structure as Bucket but stores keys only.

## `@kv` -- Key-Value Pairs

Used for map-type BTree nodes (BTree, Bucket). Contains an array of
`[key, value]` pairs.

**Small BTree (inline data):**

```json
{
  "@cls": ["BTrees.OOBTree", "OOBTree"],
  "@s": {"@kv": [["a", 1], ["b", 2], ["c", 3]]}
}
```

**Bucket:**

```json
{
  "@cls": ["BTrees.OOBTree", "OOBucket"],
  "@s": {"@kv": [["x", 10], ["y", 20]]}
}
```

**Integer-keyed BTree:**

```json
{
  "@cls": ["BTrees.IIBTree", "IIBTree"],
  "@s": {"@kv": [[1, 100], [2, 200]]}
}
```

## `@ks` -- Keys Only

Used for set-type BTree nodes (TreeSet, Set). Contains a plain array of
keys.

**TreeSet:**

```json
{
  "@cls": ["BTrees.IIBTree", "IITreeSet"],
  "@s": {"@ks": [1, 2, 3]}
}
```

**Set:**

```json
{
  "@cls": ["BTrees.OOBTree", "OOSet"],
  "@s": {"@ks": ["a", "b", "c"]}
}
```

## `@next` -- Next Bucket Pointer

When a Bucket or Set is part of a linked list of leaf nodes in a large
BTree, the `@next` key holds a persistent reference to the next
bucket/set in the chain.

```json
{
  "@cls": ["BTrees.OOBTree", "OOBucket"],
  "@s": {
    "@kv": [["a", 1], ["b", 2]],
    "@next": {"@ref": "0000000000000003"}
  }
}
```

## `@children` + `@first` -- Large BTree Internal Nodes

When a BTree is too large for a single bucket, it splits into internal
nodes with persistent references to child buckets.

`@children`
: An array of alternating child references and separator keys. Each
  child reference is a persistent ref (`@ref`); separator keys are the
  boundary values between children.

`@first`
: A persistent reference to the first bucket in the leaf chain.

```json
{
  "@cls": ["BTrees.OOBTree", "OOBTree"],
  "@s": {
    "@children": [
      {"@ref": "0000000000000005"},
      "separator_key",
      {"@ref": "0000000000000006"}
    ],
    "@first": {"@ref": "0000000000000004"}
  }
}
```

The `@children` array has the structure:
`[child_ref, key, child_ref, key, ..., child_ref]`. The number of child
references is always one more than the number of separator keys.

## Empty BTree

An empty BTree has `null` state:

```json
{"@cls": ["BTrees.OOBTree", "OOBTree"], "@s": null}
```

## BTrees.Length

`BTrees.Length.Length` objects store a plain integer. No special markers
are needed:

```json
{"@cls": ["BTrees.Length", "Length"], "@s": 42}
```

## State Shapes Summary

The following table summarizes the pickle state shape for each node type
and how the codec represents it:

| Node Type | Pickle State | JSON Representation |
|---|---|---|
| Small BTree (map) | 4-level nested tuples | `{"@kv": [[k, v], ...]}` |
| Small TreeSet (set) | 4-level nested tuples | `{"@ks": [k, ...]}` |
| Bucket (map) | 2-level nested tuples | `{"@kv": [[k, v], ...]}` |
| Set (set) | 2-level nested tuples | `{"@ks": [k, ...]}` |
| Large BTree | persistent refs | `{"@children": [...], "@first": ref}` |
| Bucket with next | 2-level + ref | `{"@kv": [...], "@next": ref}` |
| Empty BTree | `None` | `null` |
| BTrees.Length | integer | integer |
