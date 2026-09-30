"""The Rust-side allocator (mimalloc, #24) behaves across Python threads.

mimalloc keeps a heap per thread and frees blocks that another thread
allocated through a deferred path. Python threads come and go under the
extension module's feet, so pin that decoding, encoding and the JSON path
stay correct with concurrent threads and with threads that are created and
torn down repeatedly.
"""

from concurrent.futures import ThreadPoolExecutor

import pickle
import random
import threading
import zodb_json_codec


def make_record(i):
    state = {f"k{j}": ["x" * j, j, {"n": None, "f": 1.5}] for j in range(i % 40)}
    state["text"] = 'é\n"' * (i % 300)
    return pickle.dumps(("m", "C"), protocol=3) + pickle.dumps(state, protocol=3), state


RECORDS = [make_record(i) for i in range(200)]


def churn(seed, rounds=300):
    rnd = random.Random(seed)
    for _ in range(rounds):
        record, state = RECORDS[rnd.randrange(len(RECORDS))]
        decoded = zodb_json_codec.decode_zodb_record(record)
        assert decoded["@s"] == state
        _mod, _name, js, _refs = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert js.startswith("{")
        assert zodb_json_codec.encode_zodb_record(decoded)
    return rounds


def test_concurrent_threads():
    with ThreadPoolExecutor(8) as ex:
        assert sum(ex.map(churn, range(32))) == 32 * 300


def test_thread_churn():
    # every thread gets its own allocator heap; creating and joining many of
    # them must neither leak nor crash when their blocks are freed elsewhere
    keep = []
    for i in range(50):
        t = threading.Thread(
            target=lambda: keep.append(
                zodb_json_codec.decode_zodb_record(RECORDS[i][0])
            )
        )
        t.start()
        t.join()
    assert len(keep) == 50
    # blocks allocated on threads that are gone are released from this thread
    del keep[:]
    churn(0, rounds=50)
