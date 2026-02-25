# Install zodb-json-codec

<!-- diataxis: how-to -->

## From PyPI

Pre-built wheels are available for Linux, macOS, and Windows on Python 3.10 through 3.14.

```bash
pip install zodb-json-codec
```

Or with [uv](https://docs.astral.sh/uv/):

```bash
uv pip install zodb-json-codec
```

No Rust toolchain is required when installing from wheels.

## Verify the installation

```bash
python -c "import zodb_json_codec; print(zodb_json_codec.__version__)"
```

This should print the installed version number.

You can also verify the codec works by running a quick roundtrip:

```python
from zodb_json_codec import pickle_to_json, json_to_pickle

data = pickle_to_json(b"\x80\x03}q\x00X\x01\x00\x00\x00aq\x01K\x01s.")
print(data)  # {"a": 1}
```

## Next steps

- {doc}`Build from source <build-from-source>` if you need a development build or want to contribute.
- {doc}`Integrate with zodb-pgjsonb <integrate-pgjsonb>` for PostgreSQL JSONB storage.
