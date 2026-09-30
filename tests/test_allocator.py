"""The Rust-side allocator (mimalloc, #24) under Python thread churn.

mimalloc keeps a heap per thread, created on first use and torn down by a
TLS destructor when the thread exits. All Rust allocations of the codec are
thread-confined (freed inside the call that made them, or held in the
thread's own buffers), so this is a crash smoke test for heap creation and
teardown with concurrent threads and with threads created and joined
repeatedly; it cannot detect leaks.
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
    # each thread creates its own allocator heap on first use and tears it
    # down on exit; the main thread must keep working afterwards
    keep = []
    for i in range(50):
        t = threading.Thread(
            target=lambda r: keep.append(zodb_json_codec.decode_zodb_record(r)),
            args=(RECORDS[i][0],),
        )
        t.start()
        t.join()
    assert len(keep) == 50
    churn(0, rounds=50)
