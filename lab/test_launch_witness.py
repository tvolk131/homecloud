from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
import launch_witness


class WitnessResourceTests(unittest.TestCase):
    def test_repeated_samples_close_real_database_handles_without_waiting_for_gc(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'state.db'
            connect = sqlite3.connect
            with closing(connect(path)) as db:
                db.execute('CREATE TABLE vms(record TEXT)')
                db.execute('INSERT INTO vms VALUES (?)', [json.dumps({'id': '00000000-0000-4000-8000-000000000001', 'owner': 'owner', 'observed': 'deleted', 'spec': {'name': 'hc-proof-0000000000-1', 'applications': []}})])
                db.commit()
            handles = []
            def open_readonly(*args, **kwargs):
                c = connect('file:'+str(path)+'?mode=ro', uri=True)
                handles.append(c)  # Keep references so GC cannot mask a leak.
                return c
            with patch.object(launch_witness.sqlite3, 'connect', side_effect=open_readonly):
                for _ in range(500): self.assertEqual(launch_witness.sample('owner', 'hc-proof-0000000000'), {})
            for c in handles:
                with self.assertRaises(sqlite3.ProgrammingError): c.execute('SELECT 1')


if __name__ == '__main__': unittest.main()
