import base64
import hashlib
import json
import io
import os
from pathlib import Path
import tempfile
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
        self.assertIn('needs: [plan, installers, cli]', prepare)
        self.assertIn('scripts/release.py verify', prepare)
        installers = (root/'.github/workflows/build-installers.yml').read_text()
        cli = (root/'.github/workflows/build-cli.yml').read_text()
        self.assertIn('npm run build:native', installers)
        self.assertIn('cargo check --workspace --all-targets --locked', installers)
        self.assertIn('node scripts/tauri.mjs build', installers)
        self.assertIn('cargo build --release --locked -p stream-dock-manager-cli', cli)
        self.assertIn('targets: all', prepare)
        self.assertNotIn('--require-full', (root/'.github/workflows/ci.yml').read_text())
        self.assertIn('retention-days: 30', prepare)
        self.assertIn('prepare_run_id:', publisher)
        self.assertNotIn('build-installers.yml', publisher)
        self.assertNotIn('build-cli.yml', publisher)
        self.assertNotIn('needs.source.outputs.commit', publisher)

    # Exercise the actual reusable-job guard across production and read-only rehearsal contexts
    def test_rehearsal_job_guards_and_complete_platform_matrix(self):
        from types import SimpleNamespace
        root = Path(__file__).resolve().parents[1]
        prepare = (root/'.github/workflows/prepare-release.yml').read_text()
        self.assertIn("BUILD_ONLY: ${{ github.repository != 'StreamDockManager/Manager' && 'true' || 'false' }}", prepare)
        self.assertEqual(prepare.count('mode: ${{ needs.plan.outputs.mode }}'), 2)
        self.assertIn("rehearsal:\n        if: always()", prepare)
        for result in ['PLAN_RESULT', 'INSTALLERS_RESULT', 'CLI_RESULT']:
            self.assertIn('test "$'+result+'" = success', prepare)

        self.assertIn("publication:\n        if: github.repository == 'StreamDockManager/Manager'", prepare)
        for filename in ['build-installers.yml', 'build-cli.yml']:
            workflow = (root/'.github/workflows'/filename).read_text()
            condition = next(line.strip()[4:] for line in workflow.splitlines() if line.startswith('        if:'))
            expression = condition.replace('&&', ' and ').replace('||', ' or ').replace('!github.event.created', 'not github.event.created')
            for repo, mode, tag, expected in [
                ('StreamDockManager/Manager', 'publish', 'v0.1.0', True),
                ('example/noncanonical-repository', 'build-only', '', True),
                ('example/noncanonical-repository', 'publish', 'v0.1.0', False),
                ('StreamDockManager/Manager', 'build-only', '', False),
            ]:
                github = SimpleNamespace(repository=repo, event_name='workflow_dispatch', ref='refs/heads/main', sha='a'*40,
                    workflow_ref=repo+'/.github/workflows/prepare-release.yml@refs/heads/main', event=SimpleNamespace(created=False))
                inputs = SimpleNamespace(commit='a'*40, mode=mode, tag=tag, targets='all')
                context = {'github':github, 'inputs':inputs, 'format':lambda template, value:template.format(value)}
                with self.subTest(workflow=filename, repository=repo, mode=mode):
                    self.assertEqual(bool(eval(expression, {'__builtins__':{}}, context)), expected)
                    for field, value in [('commit','b'*40), ('targets','linux')]:
                        prior = getattr(inputs,field); setattr(inputs,field,value)
                        self.assertFalse(eval(expression, {'__builtins__':{}}, context))
                        setattr(inputs,field,prior)
                    github.workflow_ref=repo+'/.github/workflows/other.yml@refs/heads/main'
                    self.assertFalse(eval(expression, {'__builtins__':{}}, context))
            defer = next(line for line in workflow.splitlines() if 'vars.DEFER_MACOS_X64' in line)
            self.assertIn("github.ref == 'refs/heads/package-build'", defer)
            self.assertIn('macos-15-intel', workflow)
            self.assertIn('inputs.commit == github.sha', condition)

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
