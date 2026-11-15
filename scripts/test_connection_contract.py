#!/usr/bin/env python3
"""Regression coverage for the executable connection-contract CI gate."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'connection_contract_gate', Path(__file__).with_name('check-connection-contract.py'))
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

REGISTRY = {
    'contract_tests': ['contract::schema'],
    'capabilities': [{'id': 'fixture-v1', 'implementation': [{'path': 'src/lib.rs', 'symbol': 'fixture'}],
                      'tests': ['capability::behavior']}],
    'vendors': [{'dialect': 'fixture', 'implementation': [{'path': 'src/lib.rs', 'symbol': 'vendor'}],
                 'tests': ['vendor::subset']}],
}
NAMES = {'contract::schema', 'capability::behavior', 'vendor::subset'}


# Create a test harness artifact with the requested selectors
def artifact(executable):
    return json.dumps({'reason': 'compiler-artifact', 'target': {'name': 'stream_dock_manager_plugin_host'},
                       'profile': {'test': True}, 'executable': executable})


class ConnectionContractGateTests(unittest.TestCase):
    # Exercise the connection-contract selector against an isolated test harness
    def exercise(self, registry=None, available=NAMES, ignored=(), binaries=('/fixture/test-binary',), fail=None):
        registry = copy.deepcopy(REGISTRY if registry is None else registry)
        calls = []

        # Run the selected command and propagate failure to the calling workflow
        def run(command, **kwargs):
            self.assertIs(kwargs['check'], True)
            calls.append(command)
            if command[0] == 'cargo':
                return subprocess.CompletedProcess(command, 0, stdout='\n'.join(map(artifact, binaries)))
            if '--list' in command:
                names = ignored if '--ignored' in command else available
                return subprocess.CompletedProcess(command, 0, stdout='\n'.join(name + ': test' for name in names))
            if fail:
                raise subprocess.CalledProcessError(1, command)
            self.assertEqual(command[1], '--exact')
            return subprocess.CompletedProcess(command, 0)

        with tempfile.TemporaryDirectory() as directory:
            host = Path(directory)
            (host / 'connection-implementations.v1.json').write_text(json.dumps(registry))
            with patch.object(GATE, 'HOST', host), patch.object(GATE.subprocess, 'run', side_effect=run):
                GATE.run_conformance()
        return calls

    # Verify valid selection executes each exact test once
    def test_valid_selection_executes_each_exact_test_once(self):
        calls = self.exercise()
        execution = [call for call in calls if '--exact' in call]
        self.assertEqual(len(execution), len(NAMES))
        self.assertEqual({call[2] for call in execution}, NAMES)
        self.assertTrue(all(call[1:] == ['--exact', call[2], '--nocapture'] for call in execution))

    # Verify missing selector fails instead of running zero tests
    def test_missing_selector_fails_instead_of_running_zero_tests(self):
        with self.assertRaisesRegex(ValueError, 'do not exist'):
            self.exercise(available=NAMES - {'vendor::subset'})

    # Verify ignored selector is not run or silently skipped
    def test_ignored_selector_is_not_run_or_silently_skipped(self):
        with self.assertRaisesRegex(ValueError, 'ignored probes'):
            self.exercise(ignored=['vendor::subset'])

    # Verify empty declarations fail
    def test_empty_declarations_fail(self):
        for field in ('contract_tests', 'capabilities', 'vendors'):
            with self.subTest(field=field):
                registry = copy.deepcopy(REGISTRY)
                registry[field] = []
                with self.assertRaisesRegex(ValueError, 'empty conformance'):
                    self.exercise(registry=registry)
        registry = copy.deepcopy(REGISTRY)
        registry['capabilities'][0]['tests'] = []
        with self.assertRaisesRegex(ValueError, 'empty conformance'):
            self.exercise(registry=registry)

    # Verify non list selection and option injection are rejected
    def test_non_list_selection_and_option_injection_are_rejected(self):
        for value in ('contract::schema', ['--help'], [None]):
            with self.subTest(value=value):
                registry = copy.deepcopy(REGISTRY)
                registry['contract_tests'] = value
                with self.assertRaises(ValueError):
                    self.exercise(registry=registry)

    # Verify no or ambiguous harness fails
    def test_no_or_ambiguous_harness_fails(self):
        for binaries in ((), ('/one', '/two')):
            with self.subTest(binaries=binaries):
                with self.assertRaisesRegex(ValueError, 'exactly one'):
                    self.exercise(binaries=binaries)

    # Verify failed behavior test fails gate
    def test_failed_behavior_test_fails_gate(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.exercise(fail=True)


if __name__ == '__main__':
    unittest.main()
