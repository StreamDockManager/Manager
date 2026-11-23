"""The warning gate must fail local diagnostics without changing dependency policy."""
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import check_syntax as checker


class SyntaxGateTests(unittest.TestCase):
    # Construct a compiler diagnostic fixture for syntax-gate tests
    def diagnostic(self, source, level='warning', **extra):
        return {'reason': 'compiler-message', 'target': {'src_path': str(source)},
                'message': {'level': level, 'message': 'fixture diagnostic',
                            'rendered': f'{level}: fixture diagnostic\n'}, **extra}

    # Run the mocked Cargo invocation and capture diagnostic handling
    def run_cargo(self, records, root, status=0):
        process = MagicMock()
        process.__enter__.return_value = process
        process.stdout = io.StringIO('\n'.join(json.dumps(record) for record in records))
        process.wait.return_value = status
        with patch.object(checker.subprocess, 'Popen', return_value=process) as launch:
            checker.check_rust(['cargo', 'check'], root)
        return launch

    # Verify workspace and local patch warnings fail including cached diagnostics
    def test_workspace_and_local_patch_warnings_fail_including_cached_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for source in [root / 'crates/runtime/src/lib.rs', root / 'vendor/.cache/sources/glib-0.18.5/src/lib.rs']:
                with self.subTest(source=source):
                    with self.assertRaisesRegex(ValueError, '1 warning'):
                        self.run_cargo([
                            {'reason': 'compiler-artifact', 'fresh': True},
                            self.diagnostic(source),
                            {'reason': 'build-finished', 'success': True},
                        ], root)

    # Verify registry dependency warnings stay visible but do not fail
    def test_registry_dependency_warnings_stay_visible_but_do_not_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'project'
            records = [self.diagnostic(Path(directory) / '.cargo/registry/src/dependency/lib.rs')]
            with patch('sys.stderr', new_callable=io.StringIO) as stderr:
                launch = self.run_cargo(records, root)
            self.assertIn('warning: fixture diagnostic', stderr.getvalue())
            self.assertNotIn('env', launch.call_args.kwargs)

    # Verify registry cache inside checkout is still external
    def test_registry_cache_inside_checkout_is_still_external(self):
        root = Path('/tmp/manager')
        for package in ['registry+https://github.com/rust-lang/crates.io-index#glib@0.18.5',
                        'git+https://example.invalid/project#dependency@1.0.0']:
            record = self.diagnostic(root / 'target/.cargo/registry/src/dependency/lib.rs',
                                     package_id=package)
            self.assertFalse(checker.owned_warning(record, root))

    # Verify compile errors and nonzero exit always fail
    def test_compile_errors_and_nonzero_exit_always_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for records in [[], [self.diagnostic(root / 'src/lib.rs', 'error')]]:
                with self.subTest(records=records), self.assertRaises(subprocess.CalledProcessError):
                    self.run_cargo(records, root, status=101)

    # Verify notes and artifacts are not warnings
    def test_notes_and_artifacts_are_not_warnings(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.run_cargo([self.diagnostic(root / 'src/lib.rs', 'note'),
                            {'reason': 'compiler-artifact', 'fresh': True}], root)

    # Verify paths do not confuse sibling directory with local source
    def test_paths_do_not_confuse_sibling_directory_with_local_source(self):
        root = Path('/tmp/manager')
        self.assertFalse(checker.owned_warning(self.diagnostic('/tmp/manager-other/lib.rs'), root))
        self.assertTrue(checker.owned_warning(self.diagnostic('vendor/.cache/sources/glib-0.18.5/src/lib.rs'), root))

    # Verify command preserves locked target selection without warning flags
    def test_command_preserves_locked_target_selection_without_warning_flags(self):
        default = checker.cargo_command()
        self.assertIn('--workspace', default)
        self.assertIn('--all-targets', default)
        self.assertIn('--locked', default)
        self.assertIn('--message-format=json', default)
        selected = checker.cargo_command(packages=['stream-dock-manager'], target='x86_64-unknown-linux-gnu', release=True)
        self.assertNotIn('--workspace', selected)
        self.assertEqual(selected[-3:], ['--target', 'x86_64-unknown-linux-gnu', '--release'])
        self.assertIn('stream-dock-manager', selected)
        self.assertNotIn('-Dwarnings', selected)
        self.assertEqual(checker.cargo_command(config='/tmp/sdk.toml')[-2:],
                         ['--config', '/tmp/sdk.toml'])

    def test_strict_clippy_retains_locked_targets_and_json_ownership_scan(self):
        self.assertEqual(
            checker.cargo_command(clippy=True),
            ['cargo', 'clippy', '--locked', '--all-targets', '--message-format=json', '--workspace',
             '--', '-D', 'warnings'],
        )

    # Verify python syntax failure does not write bytecode
    def test_python_syntax_failure_does_not_write_bytecode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / 'scripts'
            scripts.mkdir()
            (scripts / 'bad.py').write_text('def broken(:\n')
            with self.assertRaises(SyntaxError):
                checker.check_python(root)
            self.assertFalse((scripts / '__pycache__').exists())

    # Verify npm check includes gate
    def test_npm_check_includes_gate(self):
        package = json.loads((checker.ROOT / 'package.json').read_text())
        self.assertTrue(package['scripts']['check'].startswith('npm run check:syntax && '))
        self.assertEqual(package['scripts']['check:syntax'], 'node scripts/check-syntax.mjs')

    # Verify linux packaging gate precedes packaging and preserves cli independence
    def test_linux_packaging_gate_precedes_packaging_and_preserves_cli_independence(self):
        source = (checker.ROOT / 'packaging/linux/cross.Dockerfile').read_text()
        gate = source.index('python3 scripts/check_syntax.py --rust-only --release')
        self.assertLess(source.index('npm run build:native'), gate)
        self.assertLess(gate, source.index('python3 scripts/build.py package-target --linux-container'))
        self.assertIn('if [ "$PRODUCT" = cli ]; then set -- --package stream-dock-manager-cli;', source)
        self.assertIn('if [ "$PRODUCT" = all ]; then set -- "$@" --package stream-dock-manager-cli;', source)
        self.assertIn('--target x86_64-unknown-linux-gnu "$@"', source)


if __name__ == '__main__':
    unittest.main()
