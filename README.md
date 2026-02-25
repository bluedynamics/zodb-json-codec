# zodb-json-codec

Fast pickle-to-JSON transcoder for ZODB, implemented in Rust via PyO3.

Converts ZODB pickle records into human-readable, JSONB-queryable JSON
while maintaining full roundtrip fidelity. Designed as the codec layer for
[zodb-pgjsonb](https://github.com/bluedynamics/zodb-pgjsonb), a PostgreSQL
JSONB storage backend for ZODB.

**Key capabilities:**

- Full roundtrip fidelity: encode to JSON and back produces identical pickle bytes
- Human-readable JSON with compact type markers (`@dt`, `@ref`, `@kv`, ...)
- JSONB-queryable output for PostgreSQL
- Faster than CPython's C pickle extension on most operations
- GIL released during Rust phases for multi-threaded Python
- Escape hatch (`@pkl`) ensures any pickle data roundtrips safely

## Installation

```bash
pip install zodb-json-codec
```

For building from source, see the
[documentation](https://bluedynamics.github.io/zodb-json-codec/how-to/build-from-source.html).

## Quick Example

```python
import zodb_json_codec

# ZODB records (two concatenated pickles: class + state)
record = zodb_json_codec.decode_zodb_record(data)
# -> {"@cls": ["myapp.models", "Document"], "@s": {"title": "Hello", ...}}
data = zodb_json_codec.encode_zodb_record(record)

# Standalone pickle <-> JSON string
json_str = zodb_json_codec.pickle_to_json(pickle_bytes)
pickle_bytes = zodb_json_codec.json_to_pickle(json_str)
```

## Documentation

Full documentation is available at
**https://bluedynamics.github.io/zodb-json-codec/**

- [Tutorials](https://bluedynamics.github.io/zodb-json-codec/tutorials/) — Getting started, working with ZODB records
- [How-To Guides](https://bluedynamics.github.io/zodb-json-codec/how-to/) — Install, integrate, benchmark, build from source
- [Reference](https://bluedynamics.github.io/zodb-json-codec/reference/) — Python API, JSON format, BTree format, project structure
- [Explanation](https://bluedynamics.github.io/zodb-json-codec/explanation/) — Architecture, performance, optimization journal, security

## Source Code and Contributions

The source code is managed in a Git repository, with its main branches hosted on GitHub.
Issues can be reported there too.

We'd be happy to see many forks and pull requests to make this package even better.
We welcome AI-assisted contributions, but expect every contributor to fully understand and be able to explain the code they submit.
Please don't send bulk auto-generated pull requests.

Maintainers are Jens Klein and the BlueDynamics Alliance developer team.
We appreciate any contribution and if a release on PyPI is needed, please just contact one of us.
We also offer commercial support if any training, coaching, integration or adaptations are needed.

## License

MIT
