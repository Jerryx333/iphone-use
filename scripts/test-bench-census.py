#!/usr/bin/env python3
"""Run the actual census CLI against loopback faults; never operate a phone."""
import json
import os
from pathlib import Path
import subprocess
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SCRIPT = Path(__file__).with_name('bench-census.py')


class CensusTests(unittest.TestCase):
    def run_census(self, fault=None):
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.respond()

            def do_POST(self):
                self.respond()

            def respond(self):
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
                requests.append((self.command, self.path, body, self.headers.get('X-Phone-Owner')))
                status, reply = 200, {'ok': True}
                if self.path == '/agent/status':
                    reply = {'drivable': True, 'version': 'fixture', 'owner': 'bench-census' if fault == 'owned' else None}
                elif self.path == '/agent/elements':
                    reply = {'elements': [], 'snapshot': 'fixture-snapshot'}
                    if fault == 'read':
                        status, reply = 502, {'error': 'read_failed'}
                    elif fault == 'missing-tree':
                        reply = {'ok': True}
                    elif fault == 'missing-snapshot':
                        reply = {'elements': []}
                    elif fault == 'invalid-tree':
                        reply = {'elements': {}, 'snapshot': 'fixture-snapshot'}
                elif self.path == '/agent/input' and body.get('type') == 'tap':
                    if fault == 'unknown':
                        reply = {'ok': False, 'outcome': 'unknown', 'retry_safe': False}
                    elif fault == 'http':
                        status, reply = 502, {'error': 'incomplete'}
                    elif fault == 'missing-ack':
                        reply = {'timing': {}}
                    elif fault == 'unreadable':
                        reply = b'<html>gateway</html>'
                    elif fault == 'disconnect':
                        self.close_connection = True
                        return
                elif self.path == '/agent/input' and fault == 'warmup':
                    reply = {'ok': False, 'error': 'launch_failed'}
                elif self.path == '/agent/hold' and body.get('secs') == 900 and fault == 'hold':
                    status, reply = 409, {'ok': False}
                elif self.path == '/agent/hold' and body.get('secs') == 0 and fault == 'cleanup':
                    status, reply = 500, {'ok': False}
                elif self.path == '/agent/screenshot':
                    reply = {'ok': True} if fault == 'json-image' else b'\x89PNG\r\n\x1a\nfixture'
                raw = reply if isinstance(reply, bytes) else json.dumps(reply).encode()
                self.send_response(status)
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            env = dict(os.environ, IPHONE_USE_URL=f'http://127.0.0.1:{server.server_port}', IPHONE_USE_TOKEN='fixture-only')
            try:
                result = subprocess.run(['python3', str(SCRIPT), '--runs', '3'], env=env, capture_output=True, text=True, timeout=15)
            finally:
                server.shutdown()
                thread.join()
        return result, requests

    def test_unknown_or_unreadable_mutation_is_not_repeated(self):
        for fault in ('unknown', 'http', 'missing-ack', 'unreadable', 'disconnect'):
            with self.subTest(fault=fault):
                result, requests = self.run_census(fault)
                self.assertEqual(result.returncode, 1, result.stderr)
                actions = [r[2]['type'] for r in requests if r[1] == '/agent/input']
                self.assertEqual(actions.count('tap'), 1)
                self.assertNotIn('key', actions)
                self.assertNotIn('swipe', actions)
                self.assertEqual([r[1] for r in requests[-2:]], ['/agent/hold', '/agent/owner'])
                self.assertIn('stopped without replay', result.stderr)

    def test_unconfirmed_tree_prevents_the_following_tap(self):
        for fault in ('missing-tree', 'missing-snapshot', 'invalid-tree'):
            with self.subTest(fault=fault):
                result, requests = self.run_census(fault)
                self.assertEqual(result.returncode, 1)
                failed_read = next(i for i, r in enumerate(requests) if r[1] == '/agent/elements')
                self.assertFalse(any(r[1] == '/agent/input' for r in requests[failed_read + 1:]))

    def test_json_acknowledgement_is_not_a_screenshot(self):
        result, requests = self.run_census('json-image')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(sum(r[1] == '/agent/screenshot' for r in requests), 1)
        self.assertFalse(any(r[2].get('type') == 'shortcut' for r in requests))

    def test_failed_read_prevents_the_following_tap(self):
        result, requests = self.run_census('read')
        self.assertEqual(result.returncode, 1)
        failed_read = next(i for i, r in enumerate(requests) if r[1] == '/agent/elements')
        self.assertFalse(any(r[1] == '/agent/input' for r in requests[failed_read + 1:]))

    def test_warmup_failure_releases_lease_without_further_action(self):
        result, requests = self.run_census('warmup')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(sum(r[1] == '/agent/input' for r in requests), 1)
        self.assertEqual(requests[-1][1], '/agent/owner')

    def test_refused_hold_does_not_claim_again(self):
        result, requests = self.run_census('hold')
        self.assertEqual(result.returncode, 1)
        self.assertFalse(any(r[1] == '/agent/input' for r in requests))
        self.assertEqual([r[2] for r in requests if r[1] == '/agent/hold'], [{'secs': 900}])

    def test_other_census_owner_is_not_reused(self):
        result, requests = self.run_census('owned')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(requests), 1)

    def test_cleanup_failure_still_attempts_owner_release(self):
        result, requests = self.run_census('cleanup')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(requests[-1][1], '/agent/owner')
        self.assertIn('cleanup:', result.stderr)

    def test_success_keeps_all_samples_but_does_not_claim_task_verification(self):
        result, requests = self.run_census()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(r[2].get('type') == 'tap' for r in requests), 9)
        self.assertEqual(sum(r[1] == '/agent/screenshot' for r in requests), 3)
        self.assertIn('not independently verified', result.stdout)
        self.assertEqual(len(set(r[3] for r in requests)), 1)
        self.assertRegex(requests[0][3], r'^bench-census-[a-f0-9]{32}$')


if __name__ == '__main__':
    unittest.main()
