import base64
import hashlib
import json
import io
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch, MagicMock
import zipfile

import prepared_release as p
import release


class PreparedReleaseTests(unittest.TestCase):
    # Create isolated fixtures for PreparedReleaseTests checks
    def setUp(self):
        self.env = patch.dict(os.environ, {'GITHUB_REPOSITORY': 'StreamDockManager/Manager',
            'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_REF_NAME': 'main',
            'BUILD_ONLY': 'false'})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.info = {'id': 12, 'workflow_id': 4, 'path': p.WORKFLOW, 'event': 'workflow_dispatch',
            'head_branch': 'main', 'head_sha': 'a' * 40, 'run_attempt': 2,
            'status': 'completed', 'conclusion': 'success', 'run_started_at': '2026-11-14T01:00:00Z',
            'repository': {'full_name': 'StreamDockManager/Manager'},
            'head_repository': {'full_name': 'StreamDockManager/Manager'}}
        self.workflow = {'id': 4, 'path': p.WORKFLOW}
        self.artifact = {'id': 77, 'name': p.ARTIFACT, 'expired': False, 'size_in_bytes': 100,
            'digest': 'sha256:' + 'b'*64, 'created_at': '2026-11-14T02:00:00Z',
            'workflow_run': {'id': 12, 'head_sha': 'a'*40, 'head_branch': 'main'}}

    # Verify successful main workflow identity
    def test_successful_main_workflow_identity(self):
        with patch.object(p, 'api', side_effect=[self.info, self.workflow]):
            self.assertEqual(p.preparation('12'), self.info)
        mutations = {'id': 13, 'workflow_id': 5, 'path': '.github/workflows/ci.yml', 'event': 'push',
            'head_branch': 'other', 'head_sha': 'bad', 'run_attempt': 0, 'status': 'in_progress',
            'conclusion': 'failure', 'repository': {'full_name':'other/repo'},
            'head_repository': {'full_name':'fork/repo'}}
        for field, value in mutations.items():
            with self.subTest(field=field), patch.object(p, 'api', side_effect=[{**self.info, field:value}, self.workflow]):
                with self.assertRaises(ValueError): p.preparation('12')
        for invalid in ['', '0', '-1', '12/../3', 'abc']:
            with self.assertRaises(ValueError): p.preparation(invalid)

    # Verify artifact must match current successful attempt and digest
    def test_artifact_must_match_current_successful_attempt_and_digest(self):
        # Check the mocked publication step before allowing the test to continue
        def check(items):
            with patch.object(p, 'api', return_value={'total_count':len(items), 'artifacts':items}):
                return p.publication_artifact(self.info)
        self.assertEqual(check([self.artifact]), self.artifact)
        for items in [[], [self.artifact, self.artifact]]:
            with self.assertRaises(ValueError): check(items)
        for key, value in [('expired',True), ('digest',None), ('size_in_bytes',p.MAX_ARCHIVE+1),
                           ('created_at','2026-11-14T00:00:00Z'), ('workflow_run',{'id':99})]:
            with self.subTest(key=key), self.assertRaises(ValueError): check([{**self.artifact,key:value}])

    # Verify source version is read at authenticated sha
    def test_source_version_is_read_at_authenticated_sha(self):
        with patch.object(p, 'api', return_value={'type':'file','encoding':'base64','content':base64.b64encode(b'0.1.0\n').decode()}) as call:
            self.assertEqual(p.source_version(self.info),'0.1.0')
            self.assertIn('ref='+'a'*40,call.call_args.args[0])

    # Verify extract rejects traversal nested duplicate symlink and oversize
    def test_extract_rejects_traversal_nested_duplicate_symlink_and_oversize(self):
        for names in [['../escape'], ['nested/file'], ['same','same'], ['/absolute'], ['a\\b']]:
            with tempfile.TemporaryDirectory() as tmp:
                archive=Path(tmp)/'a.zip'
                with zipfile.ZipFile(archive,'w') as z:
                    for name in names: z.writestr(name,b'bytes')
                with self.assertRaises(ValueError): p.extract(archive,Path(tmp)/'out')
                self.assertFalse((Path(tmp)/'out').exists())
        with tempfile.TemporaryDirectory() as tmp:
            archive=Path(tmp)/'a.zip'
            with zipfile.ZipFile(archive,'w') as z:
                entry=zipfile.ZipInfo('link'); entry.external_attr=0o120777<<16
                z.writestr(entry,'target')
            with self.assertRaises(ValueError): p.extract(archive,Path(tmp)/'out')
        with tempfile.TemporaryDirectory() as tmp:
            archive=Path(tmp)/'a.zip'
            with zipfile.ZipFile(archive,'w') as z: z.writestr('catalog.json',b'12345')
            with patch.object(p,'MAX_METADATA',4), self.assertRaises(ValueError): p.extract(archive,Path(tmp)/'out')

    # Verify extract preserves exact bytes and refuses existing destination
    def test_extract_preserves_exact_bytes_and_refuses_existing_destination(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive=Path(tmp)/'a.zip'; out=Path(tmp)/'out'
            with zipfile.ZipFile(archive,'w') as z: z.writestr('installer.deb',b'original')
            p.extract(archive,out)
            self.assertEqual((out/'installer.deb').read_bytes(),b'original')
            with self.assertRaises(ValueError): p.extract(archive,out)

    # Verify standalone notices survive preparation without becoming updater downloads
    def test_standalone_notices_are_prepared_verified_and_kept_out_of_catalogues(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {
                'PREPARED_RELEASE_VERSION': '0.1.0', 'RELEASE_COMMIT': 'a'*40, 'RELEASE_BUILD': '12'}):
            root = Path(tmp)
            repo = root / 'source'
            repo.mkdir()
            data = b'# Third-party notices\n\nExample terms\n'
            (repo / release.NOTICES).write_bytes(data)
            native = root / 'native'
            native.mkdir()
            identity = {'schema_version': 1, 'repository': 'StreamDockManager/Manager',
                        'version': '0.1.0', 'release_tag': 'v0.1.0', 'channel': 'stable',
                        'commit': 'a'*40, 'build': '12', 'published_at': '2026-11-14T02:00:00Z',
                        'packages': []}
            for product, variants, filename in [
                    (release.PRODUCT, release.VARIANTS, 'catalog-fixture.json'),
                    (release.CLI_PRODUCT, release.CLI_VARIANTS, 'cli-catalog-fixture.json')]:
                assets = []
                for platform, arch, extension in sorted(variants):
                    name = f'stream-dock-{product}-0.1.0-{platform}-{arch}{extension}'
                    path = native / name
                    path.write_bytes(name.encode('ascii'))
                    assets.append({'name': name, 'os': platform, 'arch': arch, 'signing': 'unsigned',
                                   'url': f'https://github.com/StreamDockManager/Manager/releases/download/v0.1.0/{name}',
                                   'size': path.stat().st_size, 'sha256': release.file_digest(path)})
                (native / filename).write_text(json.dumps({**identity, 'product': product, 'assets': assets}))
            with patch.object(release, 'ROOT', repo):
                output = release.assemble(native)
            self.assertEqual((output / release.NOTICES).read_bytes(), data)
            self.assertIn(f'{hashlib.sha256(data).hexdigest()}  {release.NOTICES}',
                          (output / 'SHA256SUMS').read_text().splitlines())
            for filename in ('catalog.json', 'cli-catalog.json'):
                self.assertNotIn(release.NOTICES, {asset['name'] for asset in json.loads((output / filename).read_text())['assets']})
            archive = root / 'publication.zip'
            with zipfile.ZipFile(archive, 'w') as bundle:
                for path in output.iterdir():
                    bundle.write(path, path.name)
            extracted = root / 'extracted'
            p.extract(archive, extracted)
            catalog = release.verify_publication(extracted)
            self.assertIn(f'/blob/{identity["commit"]}/{release.NOTICES}', release.release_notes(catalog))
            notices = extracted / release.NOTICES
            notices.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'inventory/checksums'):
                release.verify_publication(extracted)
            notices.unlink()
            with self.assertRaisesRegex(ValueError, f'Missing {release.NOTICES}'):
                release.verify_publication(extracted)

    def test_release_target_profiles_are_explicit_complete_and_bound_to_both_catalogues(self):
        source_env = {'PREPARED_RELEASE_VERSION': '0.1.0', 'RELEASE_COMMIT': 'a'*40, 'RELEASE_BUILD': '12',
                      'RELEASE_TARGET_PROFILE': 'standard'}
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, source_env):
            root = Path(tmp)
            native = root / 'native'
            native.mkdir()
            (root / release.NOTICES).write_text('Notices')
            identity = {'schema_version': 1, 'repository': 'StreamDockManager/Manager', 'version': '0.1.0',
                        'release_tag': 'v0.1.0', 'channel': 'stable', 'commit': 'a'*40, 'build': '12',
                        'published_at': '2026-11-14T02:00:00Z', 'packages': []}
            fragments = []
            for product, variants, name in [(release.PRODUCT, release.VARIANTS, 'catalog-fixture.json'),
                                            (release.CLI_PRODUCT, release.CLI_VARIANTS, 'cli-catalog-fixture.json')]:
                assets = []
                for platform, arch, extension in sorted(variants):
                    if (platform, arch) == ('macos','amd64'):
                        continue
                    filename = f'stream-dock-{product}-0.1.0-{platform}-{arch}{extension}'
                    path = native / filename
                    path.write_bytes(filename.encode('ascii'))
                    assets.append({'name':filename, 'os':platform, 'arch':arch, 'signing':'unsigned',
                                   'url':f'https://github.com/StreamDockManager/Manager/releases/download/v0.1.0/{filename}',
                                   'size':path.stat().st_size, 'sha256':release.file_digest(path)})
                fragment = {**identity, 'product':product, 'assets':assets}
                (native / name).write_text(json.dumps(fragment))
                fragments.append((name, fragment))
            with patch.object(release, 'ROOT', root):
                output = release.assemble(native)
                self.assertEqual(release.verify_publication(output)['target_profile'], 'standard')
                with patch.dict(os.environ, {'RELEASE_TARGET_PROFILE': 'with-intel'}):
                    with self.assertRaisesRegex(ValueError, 'target profile differs'):
                        release.verify_publication(output)
                    with self.assertRaisesRegex(ValueError, 'exactly the supported'):
                        release.assemble(native)
                with patch.dict(os.environ):
                    os.environ.pop('RELEASE_TARGET_PROFILE')
                    self.assertEqual(release.verify_publication(output)['target_profile'], 'standard')
                name, fragment = fragments[1]
                (native / name).write_text(json.dumps({**fragment, 'assets':fragment['assets'][:-1]}))
                with self.assertRaisesRegex(ValueError, 'exactly the supported native CLI'):
                    release.assemble(native)
                (native / name).write_text(json.dumps({**fragment, 'target_profile':'unknown'}))
                with self.assertRaisesRegex(ValueError, 'Unknown release target profile'):
                    release.assemble(native)

                # Both product catalogues must agree even when each set is independently valid
                desktop = output / 'catalog.json'
                standard_desktop = desktop.read_bytes()
                for name, fragment in fragments:
                    product = fragment['product']
                    suffixes = ('.dmg', '.zip') if product == release.CLI_PRODUCT else ('.dmg',)
                    for extension in suffixes:
                        filename = f'stream-dock-{product}-0.1.0-macos-amd64{extension}'
                        path = native / filename
                        path.write_bytes(filename.encode('ascii'))
                        fragment['assets'].append({
                            'name':filename, 'os':'macos', 'arch':'amd64', 'signing':'unsigned',
                            'url':f'https://github.com/StreamDockManager/Manager/releases/download/v0.1.0/{filename}',
                            'size':path.stat().st_size, 'sha256':release.file_digest(path),
                        })
                    (native / name).write_text(json.dumps(fragment))
                with patch.dict(os.environ, {'RELEASE_TARGET_PROFILE': 'with-intel'}):
                    output = release.assemble(native)
                    self.assertEqual(release.verify_publication(output)['target_profile'], 'with-intel')
                desktop.write_bytes(standard_desktop)
                with patch.dict(os.environ):
                    os.environ.pop('RELEASE_TARGET_PROFILE')
                    with self.assertRaisesRegex(ValueError, 'mismatch: target_profile'):
                        release.verify_publication(output)
                    no_profile = json.loads(standard_desktop)
                    del no_profile['target_profile']
                    desktop.write_text(json.dumps(no_profile))
                    with self.assertRaisesRegex(ValueError, 'exactly the supported native installer'):
                        release.verify_publication(output)

    # Verify notices use the document size bound during artifact extraction
    def test_extract_rejects_oversized_notices(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'publication.zip'
            with zipfile.ZipFile(archive, 'w') as bundle:
                bundle.writestr(release.NOTICES, b'12345')
            with patch.object(release, 'MAX_NOTICES', 4), self.assertRaisesRegex(ValueError, 'oversized'):
                p.extract(archive, Path(tmp) / 'out')

    # Verify download checks provider digest size and host
    def test_download_checks_provider_digest_size_and_host(self):
        data = b'archive bytes'
        artifact = {**self.artifact, 'size_in_bytes': len(data), 'digest': 'sha256:' + hashlib.sha256(data).hexdigest()}
        for mutation, fails in [({}, False), ({'digest': 'sha256:'+'0'*64}, True), ({'size_in_bytes':1}, True), ({'size_in_bytes':len(data)+1}, True)]:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                proc = MagicMock(); proc.stdout = io.BytesIO(data); proc.wait.return_value = 0
                context = MagicMock(); context.__enter__.return_value = proc
                with patch.object(p.subprocess,'Popen',return_value=context) as start, patch.dict(os.environ,{'GH_HOST':'untrusted.example'}):
                    if fails:
                        with self.assertRaises(ValueError): p.download({**artifact,**mutation},Path(tmp)/'a.zip')
                    else:
                        p.download(artifact,Path(tmp)/'a.zip')
                        self.assertEqual((Path(tmp)/'a.zip').read_bytes(), data)
                    self.assertEqual(start.call_args.kwargs['env']['GH_HOST'],'github.com')
                    self.assertEqual(start.call_args.args[0][-1],'repos/StreamDockManager/Manager/actions/artifacts/77/zip')

    # Verify workflow surface has no publish build or prepare write permission
    def test_workflow_surface_has_no_publish_build_or_prepare_write_permission(self):
        root = Path(__file__).resolve().parents[1]
        prepare = (root/'.github/workflows/prepare-release.yml').read_text()
        publisher = (root/'.github/workflows/publish-release.yml').read_text()
        self.assertNotIn('contents: write', prepare)
        self.assertNotIn('release.py publish', prepare)
        self.assertNotIn('uses: ./.github/workflows/ci.yml', prepare)
        self.assertNotIn('--require-full', prepare)
        self.assertIn('python scripts/vendor-compatibility.py', prepare)
        self.assertIn('needs: [plan, packages]', prepare)
        self.assertIn('scripts/release.py verify', prepare)
        installers = (root/'.github/workflows/build-installers.yml').read_text()
        cli = (root/'.github/workflows/build-cli.yml').read_text()
        self.assertNotIn('npm run build:native', installers)
        self.assertNotIn('cargo check --workspace', installers)
        self.assertIn('node scripts/tauri.mjs build', installers)
        self.assertIn('product: cli', cli)
        self.assertIn('cargo build --release --locked -p stream-dock-manager-cli', installers)
        self.assertIn('product: all', prepare)
        self.assertEqual(prepare.count('uses: ./.github/workflows/build-installers.yml'), 1)
        self.assertIn('targets: all', prepare)
        self.assertNotIn('--require-full', (root/'.github/workflows/ci.yml').read_text())
        self.assertIn('retention-days: 30', prepare)
        self.assertIn('prepare_run_id:', publisher)
        self.assertNotIn('build-installers.yml', publisher)
        self.assertNotIn('build-cli.yml', publisher)
        self.assertNotIn('needs.source.outputs.commit', publisher)

    # Keep product-only work out of the other lane and preserve both provenance uploads
    def test_native_worker_reuses_target_and_keeps_product_outputs_separate(self):
        root = Path(__file__).resolve().parents[1]
        source = (root / '.github/workflows/build-installers.yml').read_text()
        self.assertEqual(source.count('CARGO_TARGET_DIR: '), 1)
        self.assertEqual(source.count('actions/checkout@'), 1)
        self.assertEqual(source.count('dtolnay/rust-toolchain@'), 1)
        cli_command = 'cargo build --release --locked -p stream-dock-manager-cli'
        self.assertEqual(source.count(cli_command), 1)
        self.assertIn("if: inputs.product == 'cli'\n              run: " + cli_command, source)
        self.assertIn("id: desktop-cache\n              if: inputs.product != 'cli'", source)
        self.assertEqual(source.count('node scripts/tauri.mjs build '), 1)
        self.assertLess(source.index('node scripts/tauri.mjs build'), source.index('python scripts/package_cli.py'))
        self.assertIn("inputs.product == 'all' && steps.desktop-compile.outcome == 'success'", source)
        self.assertIn("if: ${{ !cancelled() && steps.desktop-compile.outcome == 'success' }}", source)
        self.assertEqual(source.count('node scripts/tauri.mjs bundle '), 1)
        self.assertEqual(source.count('actions/setup-node@'), 1)
        self.assertIn("steps.desktop-cache.outcome == 'success'", source)
        self.assertIn("steps.desktop-catalog.outcome == 'success'", source)
        self.assertIn("'manager-cli' || 'manager-cli-development'", source)
        self.assertIn("'manager' || 'manager-development'", source)
        self.assertIn('target/release/cli/build-environment-linux-amd64.json', source)
        self.assertIn('target/release/bundle/build-environment-linux-amd64.json', source)
        self.assertEqual(json.loads((root / 'src-tauri/tauri.conf.json').read_text())['build']['beforeBuildCommand'],
                         'npm run build:native')

    # Run the actual workflow step with a fake Node executable to verify the Cargo arguments
    def test_desktop_compile_selects_both_packages_only_for_a_combined_build(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / '.github/workflows/build-installers.yml').read_text()
        section = workflow.split('id: desktop-compile', 1)[1].split('            - name:', 1)[0]
        script = textwrap.dedent(section.split('run: |\n', 1)[1])
        base = ['scripts/tauri.mjs', 'build', '--config', 'src-tauri/tauri.release.conf.json',
                '--no-bundle', '--', '--locked']
        for product, extra in [('desktop', []), ('all', ['-p', 'stream-dock-manager', '-p',
                                                        'stream-dock-manager-cli'])]:
            with self.subTest(product=product):
                result = subprocess.check_output(
                    ['bash', '-e', '-c', 'node() { printf "%s\\n" "$@"; }\n' + script],
                    env=dict(os.environ, BUILD_PRODUCT=product), text=True,
                )
                self.assertEqual(result.splitlines(), base + extra)

    # Exercise the actual reusable-job guard across production and read-only rehearsal contexts
    def test_rehearsal_job_guards_and_complete_platform_matrix(self):
        from types import SimpleNamespace
        root = Path(__file__).resolve().parents[1]
        prepare = (root/'.github/workflows/prepare-release.yml').read_text()
        self.assertIn("BUILD_ONLY: ${{ github.repository != 'StreamDockManager/Manager' && 'true' || 'false' }}", prepare)
        self.assertEqual(prepare.count('mode: ${{ needs.plan.outputs.mode }}'), 1)
        self.assertIn("rehearsal:\n        if: always()", prepare)
        for result in ['PLAN_RESULT', 'PACKAGES_RESULT']:
            self.assertIn('test "$'+result+'" = success', prepare)

        self.assertIn("publication:\n        if: github.repository == 'StreamDockManager/Manager'", prepare)
        for filename in ['build-installers.yml', 'build-cli.yml']:
            workflow = (root/'.github/workflows'/filename).read_text()
            condition = next(line.strip()[4:] for line in workflow.splitlines() if line.startswith('        if:'))
            expression = condition.replace('&&', ' and ').replace('||', ' or ').replace('!github.event.created', 'not github.event.created').replace('!inputs.macos_intel', 'not inputs.macos_intel')
            for repo, mode, tag, expected in [
                ('StreamDockManager/Manager', 'publish', 'v0.1.0', True),
                ('example/noncanonical-repository', 'build-only', '', True),
                ('example/noncanonical-repository', 'publish', 'v0.1.0', False),
                ('StreamDockManager/Manager', 'build-only', '', False),
            ]:
                github = SimpleNamespace(repository=repo, event_name='workflow_dispatch', ref='refs/heads/main', sha='a'*40,
                    workflow_ref=repo+'/.github/workflows/prepare-release.yml@refs/heads/main', event=SimpleNamespace(created=False))
                inputs = SimpleNamespace(commit='a'*40, mode=mode, tag=tag, targets='all',
                                         runner_service='github', macos_intel=False, product='all')
                context = {'github':github, 'inputs':inputs, 'format':lambda template, value:template.format(value)}
                with self.subTest(workflow=filename, repository=repo, mode=mode):
                    self.assertEqual(bool(eval(expression, {'__builtins__':{}}, context)), expected)
                    for field, value in [('commit','b'*40), ('targets','linux')]:
                        prior = getattr(inputs,field); setattr(inputs,field,value)
                        self.assertFalse(eval(expression, {'__builtins__':{}}, context))
                        setattr(inputs,field,prior)
                    github.workflow_ref=repo+'/.github/workflows/other.yml@refs/heads/main'
                    self.assertFalse(eval(expression, {'__builtins__':{}}, context))
            if filename == 'build-installers.yml':
                defer = next(line for line in workflow.splitlines() if 'vars.DEFER_MACOS_X64' in line)
                self.assertIn("github.ref == 'refs/heads/package-build'", defer)
                self.assertIn('macos-15-intel', workflow)
            self.assertIn('inputs.commit == github.sha', condition)

    def test_native_runner_selection_and_default_optional_intel(self):
        from types import SimpleNamespace
        workflows = Path(__file__).resolve().parents[1] / '.github/workflows'
        prepare = (workflows / 'prepare-release.yml').read_text()
        self.assertRegex(prepare, r'(?s)macos_intel:\s+description:.*?type: boolean\s+default: false')
        self.assertEqual(prepare.count('macos_intel: ${{ inputs.macos_intel }}'), 1)
        self.assertIn("RELEASE_TARGET_PROFILE: ${{ inputs.macos_intel && 'with-intel' || 'standard' }}", prepare)
        validation = prepare.split('              run: |\n', 1)[1].split('            - uses:', 1)[0]
        for provider, intel, expected in [('github', False, 0), ('github', True, 0),
                                          ('blacksmith', False, 0), ('blacksmith', True, 1)]:
            result = subprocess.run(['bash', '-c', validation], check=False, capture_output=True,
                                    env={**os.environ, 'RUNNER_SERVICE': provider, 'MACOS_INTEL': str(intel).lower()})
            self.assertEqual(result.returncode, expected)
        labels = {('linux','amd64'): 'blacksmith-4vcpu-ubuntu-2404',
                  ('windows','amd64'): 'blacksmith-4vcpu-windows-2025',
                  ('macos','arm64'): 'blacksmith-6vcpu-macos-15'}
        for filename in ('build-installers.yml',):
            source = (workflows / filename).read_text()
            self.assertIn("runner_service: { required: false, type: string, default: 'github' }", source)
            self.assertIn("macos_intel: { required: false, type: boolean, default: false }", source)
            guard = next(line.strip()[4:] for line in source.splitlines() if line.startswith('        if:'))
            start = source.index('${{ fromJSON(') + len('${{ fromJSON(')
            matrix_rule = source[start:source.index(') }}', start)]
            route = next(line.split('${{ ', 1)[1].removesuffix(' }}')
                         for line in source.splitlines() if line.startswith('        runs-on: '))
            def evaluate(expression, **context):
                expression = ' '.join(expression.splitlines()).strip()
                expression = expression.replace('&&',' and ').replace('||',' or ')
                expression = expression.replace('!github.event.created','not github.event.created')
                expression = expression.replace('!inputs.macos_intel','not inputs.macos_intel')
                return eval(expression, {'__builtins__': {}}, {
                    'fromJSON': json.loads, 'format': lambda template, *args: template.format(*args),
                    **context})
            repo = 'StreamDockManager/Manager'
            sha = 'a' * 40
            github = SimpleNamespace(ref='refs/heads/main', repository=repo, sha=sha, event_name='workflow_dispatch',
                                     workflow_ref=f'{repo}/.github/workflows/prepare-release.yml@refs/heads/main')
            inputs = SimpleNamespace(commit=sha, mode='publish', tag='v0.1.0', targets='all',
                                     runner_service='github', macos_intel=False, product='all')
            ctx = {'inputs':inputs, 'github':github, 'vars':SimpleNamespace(DEFER_MACOS_X64='true')}
            self.assertTrue(evaluate(guard, **ctx))
            self.assertEqual({(r['os'], r['arch']) for r in json.loads(evaluate(matrix_rule, **ctx))}, set(labels))
            inputs.macos_intel = True
            self.assertTrue(evaluate(guard, **ctx))
            self.assertEqual({(r['os'], r['arch']) for r in json.loads(evaluate(matrix_rule, **ctx))},
                             {*labels, ('macos','amd64')})
            inputs.runner_service = 'blacksmith'
            self.assertFalse(evaluate(guard, **ctx))
            inputs.macos_intel = False
            self.assertTrue(evaluate(guard, **ctx))
            for row in json.loads(evaluate(matrix_rule, **ctx)):
                matrix = SimpleNamespace(**row)
                overridden = {row['runner_variable']: '["self-hosted", "custom"]'}
                self.assertEqual(evaluate(route, **{**ctx, 'matrix':matrix, 'vars':overridden}),
                                 labels[(row['os'], row['arch'])])
                inputs.runner_service = 'github'
                self.assertEqual(evaluate(route, **{**ctx, 'matrix':matrix, 'vars':overridden}),
                                 ['self-hosted','custom'])
                inputs.runner_service = 'blacksmith'
            github.ref = 'refs/heads/package-build'
            inputs.mode, inputs.tag, inputs.targets = 'build-only', '', 'macos-amd64'
            self.assertFalse(evaluate(guard, **ctx))
            inputs.runner_service = 'github'
            self.assertTrue(evaluate(guard, **ctx))
            self.assertEqual(json.loads(evaluate(matrix_rule, **ctx))[0]['runner'], 'macos-15-intel')

    # Exercise direct development dispatch without changing production or purpose-branch gates
    def test_manager_checks_manual_development_dispatch_is_repository_and_workflow_scoped(self):
        from types import SimpleNamespace
        workflow = Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml'
        source = workflow.read_text()
        self.assertNotIn('contents: write', source)
        self.assertNotIn('release.py publish', source)
        conditions = [line.strip()[4:] for line in source.splitlines() if line.startswith('        if:')]
        self.assertEqual(len(conditions), 2)
        sha = 'a' * 40
        fixture = 'example/development-fixture'
        canonical = 'StreamDockManager/Manager'

        def allowed(condition, *, repository=fixture, repo_id='1456092478',
                    branch='main', event='workflow_dispatch', caller='ci',
                    commit='', release=False, target='all', created=False, runner_service='github'):
            github = SimpleNamespace(
                event=SimpleNamespace(created=created), event_name=event, ref=f'refs/heads/{branch}',
                repository=repository, repository_id=repo_id, sha=sha,
                workflow_ref=f'{repository}/.github/workflows/{caller}.yml@refs/heads/{branch}',
            )
            inputs = SimpleNamespace(commit=commit, release_checks=release, targets=target,
                                     runner_service=runner_service)
            context = {'github': github, 'inputs': inputs, 'format': lambda pattern, value: pattern.format(value)}
            expression = condition.replace('&&', ' and ').replace('||', ' or ').replace('!github.event.created', 'not github.event.created').replace('!inputs.macos_intel', 'not inputs.macos_intel')
            return bool(eval(expression, {'__builtins__': {}}, context))

        for condition in conditions:
            self.assertTrue(allowed(condition))
            self.assertFalse(allowed(condition, event='push'))
            self.assertFalse(allowed(condition, event='pull_request'))
            self.assertFalse(allowed(condition, repo_id='0'))
            self.assertFalse(allowed(condition, caller='other'))
            self.assertFalse(allowed(condition, branch='feature'))
            self.assertFalse(allowed(condition, commit='b' * 40))
            self.assertTrue(allowed(condition, runner_service='blacksmith'))
            self.assertFalse(allowed(condition, runner_service='other'))
            self.assertFalse(allowed(condition, repository=canonical, repo_id='0'))
            self.assertTrue(allowed(condition, repository=canonical, repo_id='0',
                                    caller='prepare-release', release=True, commit=sha))
            self.assertFalse(allowed(condition, repository=canonical, repo_id='0',
                                     caller='prepare-release', release=False, commit=sha))
            self.assertTrue(allowed(condition, repo_id='0', branch='test-build', event='push'))
            self.assertFalse(allowed(condition, repo_id='0', branch='test-build', event='push', created=True))
        import json
        matrix_start = source.index('${{ fromJSON(') + len('${{ fromJSON(')
        matrix_end = source.index(') }}', matrix_start)
        matrix = source[matrix_start:matrix_end].strip()
        self.assertIn("options: [all, linux-windows, linux, linux-host, macos, windows]", source)
        for target, linux, expected_os in [
                ('', True, {'Darwin', 'Windows'}),
                ('all', True, {'Darwin', 'Windows'}),
                ('linux-windows', True, {'Windows'}),
                ('linux', True, set()),
                ('linux-host', True, set()),
                ('macos', False, {'Darwin'}),
                ('windows', False, {'Windows'}),
                ('other', False, set())]:
            with self.subTest(target=target):
                self.assertEqual(allowed(conditions[0], target=target), linux)
                self.assertEqual(allowed(conditions[1], target=target), bool(expected_os))
                if expected_os:
                    expression = matrix.replace('&&', ' and ').replace('||', ' or ')
                    # Parse the actual job matrix without needing a YAML package in CI
                    rows = json.loads(eval(' '.join(expression.splitlines()), {'__builtins__': {}},
                                           {'inputs': SimpleNamespace(targets=target)}))
                    self.assertEqual({row['system'] for row in rows}, expected_os)
                    self.assertEqual(len(rows), len(expected_os))
                    for row in rows:
                        if row['system'] == 'Darwin':
                            self.assertEqual(row['runner_variable'], 'MACOS_RUNNER_LABELS')
                            self.assertTrue(row['os'].startswith('macos-'))
                        else:
                            self.assertEqual(row['runner_variable'], 'WINDOWS_RUNNER_LABELS')
                            self.assertTrue(row['os'].startswith('windows-'))

        linux_job, platform_job = source.split('    platform-build:', 1)
        linux_quick = linux_job.split('- name: Run native Linux Quick tests', 1)[1].split('- name:', 1)[0]
        linux_full = linux_job.split('- name: Run native Linux Full tests', 1)[1].split('- name:', 1)[0]
        native_step = platform_job.split('- name: Run selected native workspace tests with a disposable installation key', 1)[1]
        dispatch = source.split('workflow_dispatch:', 1)[1].split('workflow_call:', 1)[0]
        self.assertRegex(dispatch, r'(?s)profile:.*?default: quick.*?options: \[quick, full\]')
        self.assertRegex(dispatch, r'(?s)focus:.*?default: workspace.*?options: \[workspace, plugin-host\]')
        full_check_gate = (
            "STREAM_DOCK_MANAGER_FULL_CHECKS: ${{ inputs.release_checks || "
            "github.event_name != 'workflow_dispatch' || inputs.profile == 'full' }}"
        )
        self.assertIn(full_check_gate, linux_job)
        self.assertIn(full_check_gate, platform_job)
        self.assertIn("if: env.STREAM_DOCK_MANAGER_FULL_CHECKS != 'true'", linux_quick)
        self.assertIn('args=(--workspace --lib --bins)', linux_quick)
        self.assertIn('args=(-p stream-dock-manager-plugin-host --lib)', linux_quick)
        self.assertIn('env -u STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE python scripts/with_test_key.py '
                      'cargo test --offline --locked', linux_quick)
        self.assertIn("if: env.STREAM_DOCK_MANAGER_FULL_CHECKS == 'true' && !inputs.release_checks", linux_full)
        self.assertIn('env -u STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE python scripts/check-connection-contract.py --workspace',
                      linux_full)
        self.assertIn('if: inputs.release_checks', linux_job.split('Run release caller connection contract conformance')[1])
        self.assertIn('run: python scripts/check_syntax.py --rust-only --clippy', linux_job)
        self.assertNotIn('run: python scripts/check_syntax.py --rust-only\n', linux_job)
        self.assertIn('EXPECTED_NATIVE_TEST_SYSTEM: ${{ matrix.system }}', platform_job)
        self.assertIn('platform.system()', platform_job)
        self.assertIn('os.environ.get("CARGO_BUILD_TARGET")', platform_job)
        self.assertIn('"$RUNNER_OS" != Linux || "$(uname -s)" != Linux', linux_job)
        self.assertIn('-n "${CARGO_BUILD_TARGET:-}"', linux_job)
        self.assertIn('os.environ.pop("STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE", None)', native_step)
        self.assertIn('if: ${{ !inputs.release_checks }}', native_step)
        self.assertIn('args += ["--lib", "--bins"]', native_step)
        self.assertIn('args = ["-p", "stream-dock-manager-plugin-host", "--lib"]', native_step)
        self.assertIn('sys.executable, "scripts/with_test_key.py", "cargo", "test", "--offline", "--locked", *args',
                      native_step)
        for step in [linux_quick, linux_full, native_step]:
            self.assertNotIn('--ignored', step)
            self.assertNotIn('--include-ignored', step)
        cache = linux_job.split('Cache checked dependency archives on the Ubuntu baseline', 1)[1].split('- name:', 1)[0]
        self.assertIn("github.repository_id == '1456092478' && !inputs.release_checks", cache)
        self.assertIn("env.STREAM_DOCK_MANAGER_LINUX_HOST != 'true'", cache)
        self.assertIn("hashFiles('Cargo.lock', 'vendor/dependencies.json')", cache)
        self.assertIn('target/.cargo/registry/cache', cache)
        self.assertIn('vendor/.cache/archives', cache)
        self.assertNotIn('restore-keys', cache)
        self.assertNotIn('registry/src', cache)
        self.assertNotIn('target/debug', cache)

    # Keep rehearsal plans untagged and reject their publication even under a manual main dispatch
    def test_rehearsal_plan_is_development_only(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {
                'GITHUB_OUTPUT':str(Path(tmp)/'out'), 'BUILD_ONLY':'true',
                'GITHUB_REPOSITORY':'example/noncanonical-repository'}), \
                patch.object(release,'versions'), patch.object(release,'version',return_value='0.1.0'), \
                patch.object(release,'run',return_value='a'*40):
            release.plan()
            self.assertEqual((Path(tmp)/'out').read_text(), 'tag=\nchannel=development\ncommit='+'a'*40+'\n')
            with self.assertRaises(ValueError):
                release.require_publication_context()

    # Permit an explicitly dispatched initial release without pretending partial compatibility is fully qualified
    def test_manual_context_and_honest_initial_release_notes(self):
        release.require_publication_context()
        notes = release.release_notes({'repository': 'StreamDockManager/Manager', 'commit': 'a'*40, 'build': '12'})
        self.assertIn(release.COMPATIBILITY_NOTICE, notes)
        self.assertIn(release.COMPATIBILITY_NOTICE, release.CATALOG_RELEASE_NOTES)
        with patch.dict(os.environ, {'BUILD_ONLY': 'true'}):
            with self.assertRaisesRegex(ValueError, 'Build-only'):
                release.require_publication_context()
        for change in [{'GITHUB_EVENT_NAME': 'push'}, {'GITHUB_REF_NAME': 'other'}, {'GITHUB_REPOSITORY': 'example/noncanonical-repository'}]:
            with self.subTest(change=change), patch.dict(os.environ, change):
                with self.assertRaisesRegex(ValueError, 'explicit main dispatch'):
                    release.require_publication_context()

    # Bind creation to the API response and exact release ID without a potentially stale list lookup
    def test_created_draft_uses_exact_id_and_preserves_payload(self):
        catalog = {'repository':'StreamDockManager/Manager', 'commit':'a'*40, 'version':'0.1.0', 'build':'12'}
        record = {'id':123, 'tag_name':'v0.1.0', 'target_commitish':'a'*40, 'draft':True, 'prerelease':False, 'assets':[]}
        calls = []
        def run(command, capture):
            calls.append(command)
            if '--input' in command:
                payload = json.loads(Path(command[-1]).read_text())
                self.assertEqual(payload['body'], release.release_notes(catalog))
                self.assertEqual(payload['target_commitish'], catalog['commit'])
                self.assertIs(payload['draft'], True)
                self.assertIs(payload['prerelease'], False)
            return json.dumps(record)
        with patch.object(release, 'run', side_effect=run), patch.object(release, 'remote_release') as listing:
            self.assertEqual(release.create_draft(catalog['repository'], 'v0.1.0', catalog), record)
            listing.assert_not_called()
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:5], ['gh','api','--method','POST','repos/StreamDockManager/Manager/releases'])

    # Reject mismatched creation responses before any upload
    def test_created_draft_rejects_wrong_identity(self):
        catalog = {'repository':'StreamDockManager/Manager', 'commit':'a'*40, 'version':'0.1.0', 'build':'12'}
        record = {'id':123, 'tag_name':'v0.1.0', 'target_commitish':'a'*40, 'draft':True, 'prerelease':False, 'assets':[]}
        for mutation in [{'id':False}, {'id':0}, {'tag_name':'other'}, {'target_commitish':'b'*40}, {'draft':False}, {'prerelease':True}, {'assets':[{'name':'unexpected'}]}]:
            with self.subTest(mutation=mutation), patch.object(release, 'run', return_value=json.dumps({**record, **mutation})) as run:
                with self.assertRaises(ValueError):
                    release.create_draft(catalog['repository'], 'v0.1.0', catalog)
                self.assertEqual(run.call_count, 1)

    # Verify preparation does not create tags
    def test_preparation_does_not_create_tags(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ,{'GITHUB_OUTPUT':str(Path(tmp)/'out')}), \
                patch.object(release,'versions'), patch.object(release,'version',return_value='0.1.0'), \
                patch.object(release,'run',return_value='a'*40) as run:
            release.plan()
            self.assertEqual(run.call_count,1)
            self.assertEqual(run.call_args.args[0],['git','rev-parse','HEAD'])
            self.assertIn('tag=v0.1.0', (Path(tmp)/'out').read_text())

    # Verify private repository identity allowed without changing visibility
    def test_private_repository_identity_allowed_without_changing_visibility(self):
        with patch.object(release,'run', return_value=json.dumps({'private':True,'visibility':'private','full_name':'StreamDockManager/Manager'})) as run:
            release.verify_repository('StreamDockManager/Manager')
            self.assertNotIn('--method',run.call_args.args[0])
        with patch.object(release,'run',return_value=json.dumps({'full_name':'other/repo'})), self.assertRaises(ValueError):
            release.verify_repository('StreamDockManager/Manager')

    # Verify publish checks gates before download or tag creation
    def test_publish_checks_gates_before_download_or_tag_creation(self):
        with patch.dict(os.environ, {'GITHUB_EVENT_NAME':'push'}), patch.object(p,'download') as dl, patch.object(p,'ensure_tag') as tag:
            with self.assertRaises(ValueError): p.publish_prepared('12')
            dl.assert_not_called(); tag.assert_not_called()

    # Verify rerun during download rejected before extraction
    def test_rerun_during_download_rejected_before_extraction(self):
        with patch.object(p,'preparation',side_effect=[self.info,{**self.info,'run_attempt':3}]), \
             patch.object(p,'publication_artifact',return_value=self.artifact), patch.object(p,'source_version',return_value='0.1.0'), \
             patch.object(p,'download'), patch.object(p,'extract') as extract, patch.object(release,'verify_repository'):
            with self.assertRaises(ValueError): p.publish_prepared('12')
            extract.assert_not_called()


if __name__ == '__main__': unittest.main()
