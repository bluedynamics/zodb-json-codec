"""Fixtures for zodb-json-codec tests."""

import pickle

import pytest


@pytest.fixture
def pickle_protocol():
    """Default pickle protocol for tests (matching typical ZODB usage)."""
    return 3
