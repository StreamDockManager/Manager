"""Regression checks for container bootstrap without mutating the host system."""
from pathlib import Path
import os
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]


def bootstrap(name):
    text = (ROOT / '.github/workflows' / name).read_text()
    section = text.split('- name: Bootstrap pinned Ubuntu build userspace', 1)[1]
    return textwrap.dedent(section.split('run: |\n', 1)[1].split('            - uses:', 1)[0])


class ContainerBootstrapTests(unittest.TestCase):
    def test_both_native_workflows_use_same_valid_bootstrap(self):
        shared = bootstrap('build-installers.yml')
        cli = (ROOT / '.github/workflows/build-cli.yml').read_text()
        self.assertIn('uses: ./.github/workflows/build-installers.yml', cli)
        self.assertIn('product: cli', cli)
        subprocess.run(['bash', '-n'], input=shared, text=True, check=True)
        self.assertLess(shared.index('zz-manager-manpages'), shared.index('apt-get update'))
        self.assertNotIn('RUSTUP_INIT_SKIP_SUDO_CHECK', shared)
        self.assertNotIn('2>/dev/null', shared)

    def test_dependency_installs_do_not_request_a_terminal(self):
        text = (ROOT / '.github/workflows/build-installers.yml').read_text()
        self.assertIn('sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y', text)
        self.assertEqual(text.count('apt-get update'), 1)

    def test_linux_system_packages_match_selected_products(self):
        source = (ROOT / '.github/workflows/build-installers.yml').read_text()
        start = source.index('- name: Install dependencies for the selected Linux packages')
        step = source[start:].split('            - id:', 1)[0]
        body = textwrap.dedent(step.split('run: |\n', 1)[1])
        desktop = {'libwebkit2gtk-4.1-dev', 'libappindicator3-dev', 'librsvg2-dev', 'patchelf', 'ffmpeg'}
        cli = {'dpkg-dev', 'rpm'}
        common = {'libssl-dev', 'libudev-dev', 'libxdo-dev', 'libdbus-1-dev'}
        for product, expected in [('desktop', common | desktop), ('cli', common | cli),
                                  ('all', common | desktop | cli)]:
            with self.subTest(product=product):
                result = subprocess.check_output(
                    ['bash', '-e', '-c', "sudo() { printf '%s\\n' \"$@\"; }\n" + body],
                    env=dict(os.environ, BUILD_PRODUCT=product), text=True,
                )
                arguments = result.splitlines()
                self.assertEqual(arguments[:5], ['env', 'DEBIAN_FRONTEND=noninteractive', 'apt-get', 'install', '-y'])
                self.assertEqual(set(arguments[5:]), expected)

    def exercise_account_home(self, home):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            body = bootstrap('build-installers.yml')
            # Run the actual new shell block; only remap its system configuration
            # destination into a disposable fixture. Account lookup is mocked.
            body = body[body.index('container_home='):body.index('apt-get update')]
            body = body.replace('/etc/dpkg/dpkg.cfg.d/zz-manager-manpages', str(root / 'manpages'))
            prefix = 'getent() { printf "fixture:x:0:0::%s:/bin/bash\\n" "$ACCOUNT_HOME"; }\n'
            env = dict(os.environ, ACCOUNT_HOME=home, GITHUB_ENV=str(root / 'env'))
            result = subprocess.run(['bash', '-e', '-o', 'pipefail'], input=prefix + body,
                                    text=True, capture_output=True, env=env)
            saved = (root / 'env').read_text() if (root / 'env').exists() else ''
            config = (root / 'manpages').read_text() if (root / 'manpages').exists() else ''
            return result.returncode, saved, config

    def test_uses_verified_account_home_and_keeps_manpage_payloads(self):
        with tempfile.TemporaryDirectory() as home:
            code, saved, config = self.exercise_account_home(home)
            self.assertEqual(code, 0)
            self.assertEqual(saved, f'HOME={home}\n')
            self.assertEqual(config, 'path-include=/usr/share/man/*\n')

    def test_invalid_account_home_fails_before_export(self):
        for home in ['', '/nonexistent-manager-ci-home']:
            with self.subTest(home=home):
                code, saved, config = self.exercise_account_home(home)
                self.assertNotEqual(code, 0)
                self.assertEqual(saved, '')
                self.assertEqual(config, '')


if __name__ == '__main__':
    unittest.main()
