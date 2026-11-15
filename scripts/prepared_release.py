#!/usr/bin/env python3
"""Download and publish only an authenticated, successful main preparation's bytes."""
import argparse
import base64
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import zipfile

import release

WORKFLOW = '.github/workflows/prepare-release.yml'
ARTIFACT = 'manager-publication'
MAX_ARCHIVE = 16 * 1024**3
MAX_FILE = 2 * 1024**3
MAX_METADATA = 4 * 1024**2
MAX_FILES = 32


# Call the GitHub API endpoint with the selected request parameters
def api(path):
    raw = release.run(['gh', 'api', path], True)
    if len(raw) > MAX_METADATA:
        raise ValueError('GitHub metadata exceeds limit')
    return json.loads(raw)


# Require a successful main preparation run from the expected workflow and repository
def preparation(run_id):
    if not re.fullmatch(r'[1-9][0-9]*', run_id):
        raise ValueError('prepare_run_id must be a positive workflow run ID')
    repo = os.environ['GITHUB_REPOSITORY']
    info = api(f'repos/{repo}/actions/runs/{run_id}')
    workflow = api(f'repos/{repo}/actions/workflows/prepare-release.yml')
    if (not isinstance(info, dict) or not isinstance(workflow, dict)
            or type(info.get('id')) is not int or str(info['id']) != run_id
            or type(workflow.get('id')) is not int or info.get('workflow_id') != workflow['id']
            or workflow.get('path') != WORKFLOW or info.get('path') != WORKFLOW
            or info.get('event') != 'workflow_dispatch' or info.get('head_branch') != 'main'
            or info.get('status') != 'completed' or info.get('conclusion') != 'success'
            or type(info.get('run_attempt')) is not int or info['run_attempt'] < 1
            or not re.fullmatch(r'[0-9a-f]{40}', str(info.get('head_sha', '')))
            or any(not isinstance(info.get(key), dict) or info[key].get('full_name', '').lower() != repo.lower()
                   for key in ('repository', 'head_repository'))):
        raise ValueError('Run is not a successful main Prepare Release in this repository')
    return info


# Select the unique unexpired publication artifact with matching run identity
def publication_artifact(info):
    repo = os.environ['GITHUB_REPOSITORY']
    data = api(f'repos/{repo}/actions/runs/{info["id"]}/artifacts?per_page=100')
    if (not isinstance(data, dict) or type(data.get('total_count')) is not int
            or not isinstance(data.get('artifacts'), list) or data['total_count'] != len(data['artifacts'])
            or len(data['artifacts']) > 100):
        raise ValueError('Artifact inventory is incomplete or exceeds limit')
    matches = [a for a in data['artifacts'] if isinstance(a, dict) and a.get('name') == ARTIFACT]
    if len(matches) != 1:
        raise ValueError('Prepared publication artifact is missing or ambiguous')
    a = matches[0]
    origin = a.get('workflow_run', {})
    if (type(a.get('id')) is not int or a['id'] <= 0 or a.get('expired') is not False
            or type(a.get('size_in_bytes')) is not int or not 0 < a['size_in_bytes'] <= MAX_ARCHIVE
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', str(a.get('digest', '')))
            or not isinstance(origin, dict) or origin.get('id') != info['id']
            or origin.get('head_sha') != info['head_sha'] or origin.get('head_branch') != 'main'):
        raise ValueError('Artifact identity, digest, size or retention is invalid')
    try:
        created, started = (datetime.fromisoformat(value.replace('Z', '+00:00')) for value in (a['created_at'], info['run_started_at']))
        if created.tzinfo is None or started.tzinfo is None or created < started:
            raise ValueError('Artifact belongs to an earlier run attempt')
    except (KeyError, TypeError, AttributeError) as exc:
        raise ValueError('Artifact attempt timestamp is invalid') from exc
    return a


# Read the product version from the authenticated preparation commit
def source_version(info):
    repo = os.environ['GITHUB_REPOSITORY']
    data = api(f'repos/{repo}/contents/VERSION?ref={info["head_sha"]}')
    if not isinstance(data, dict) or data.get('type') != 'file' or data.get('encoding') != 'base64':
        raise ValueError('Prepared source VERSION is unavailable')
    value = base64.b64decode(data.get('content', ''), validate=False).decode('ascii').strip()
    if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', value):
        raise ValueError('Prepared source VERSION is invalid')
    return value


# Download the selected artifact while checking its allowed source and integrity
def download(artifact, destination):
    repo = os.environ['GITHUB_REPOSITORY']
    # gh authenticates to GitHub and follows its signed asset redirect. Never put
    # credentials in a URL or use a URL supplied by the artifact itself
    with subprocess.Popen(['gh', 'api', f'repos/{repo}/actions/artifacts/{artifact["id"]}/zip'], stdout=subprocess.PIPE, env={**os.environ, 'GH_HOST': 'github.com'}) as process:
        digest, size = hashlib.sha256(), 0
        try:
            with destination.open('xb') as output:
                while chunk := process.stdout.read(1024 * 1024):
                    size += len(chunk)
                    if size > MAX_ARCHIVE or size > artifact['size_in_bytes']:
                        raise ValueError('Artifact download exceeds declared size')
                    digest.update(chunk)
                    output.write(chunk)
            if process.wait() != 0:
                raise ValueError('GitHub artifact download failed')
        except BaseException:
            process.kill()
            process.wait()
            raise
    if size != artifact['size_in_bytes'] or 'sha256:' + digest.hexdigest() != artifact['digest']:
        raise ValueError('GitHub artifact checksum or size mismatch')


# Validate archive entries before extracting them into the owned destination
def extract(archive, destination):
    if destination.exists() or destination.is_symlink():
        raise ValueError('Artifact destination must be new')
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        if not 0 < len(entries) <= MAX_FILES or sum(e.file_size for e in entries) > MAX_ARCHIVE:
            raise ValueError('Expanded publication exceeds limits')
        names = set()
        for entry in entries:
            name = entry.filename
            mode = entry.external_attr >> 16
            if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name) or entry.orig_filename != name or name in names
                    or entry.is_dir() or entry.flag_bits & 1 or stat.S_IFMT(mode) not in (0, stat.S_IFREG)
                    or not 0 < entry.file_size <= (MAX_METADATA if name.endswith('.json') or name == 'SHA256SUMS' else MAX_FILE)):
                raise ValueError('Unsafe or oversized publication entry')
            names.add(name)
        destination.mkdir()
        for entry in entries:
            with bundle.open(entry) as source, (destination / entry.filename).open('xb') as output:
                size = 0
                while chunk := source.read(1024 * 1024):
                    size += len(chunk)
                    if size > entry.file_size:
                        raise ValueError('Expanded entry exceeds declared size')
                    output.write(chunk)
                if size != entry.file_size:
                    raise ValueError('Expanded entry is truncated')


# Create the release tag only if absent and reject conflicting tag targets
def ensure_tag(catalog):
    repo, tag, commit = catalog['repository'], catalog['release_tag'], catalog['commit']
    refs = api(f'repos/{repo}/git/matching-refs/tags/{tag}')
    if not isinstance(refs, list):
        raise ValueError('Invalid tag inventory')
    matches = [ref for ref in refs if isinstance(ref, dict) and ref.get('ref') == f'refs/tags/{tag}']
    if len(matches) > 1:
        raise ValueError('Ambiguous release tag')
    if not matches:
        release.run(['gh', 'api', '--method', 'POST', f'repos/{repo}/git/refs', '-f', f'ref=refs/tags/{tag}', '-f', f'sha={commit}'])
    release.verify_remote_tag(repo, tag, commit)


# Verify a completed preparation and publish its exact artifact bytes without rebuilding
def publish_prepared(run_id):
    if os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch' or os.environ.get('GITHUB_REF_NAME') != 'main':
        raise ValueError('Publish Release must be manually dispatched from main')
    release.require_publication_context()
    release.verify_repository(os.environ['GITHUB_REPOSITORY'])
    info = preparation(run_id)
    artifact = publication_artifact(info)
    value = source_version(info)
    with tempfile.TemporaryDirectory(prefix='manager-prepared-') as temp:
        archive, directory = Path(temp) / 'artifact.zip', Path(temp) / 'publication'
        download(artifact, archive)
        # Re-check provider identity after downloading to reject concurrent reruns
        current = preparation(run_id)
        if any(current[key] != info[key] for key in ('head_sha', 'run_attempt', 'workflow_id', 'run_started_at')):
            raise ValueError('Preparation changed during download')
        extract(archive, directory)
        os.environ.update(RELEASE_COMMIT=info['head_sha'], RELEASE_BUILD=run_id, PREPARED_RELEASE_VERSION=value)
        catalog = release.verify_publication(directory)
        ensure_tag(catalog)
        release.publish(directory)


# Parse options and run the prepared release workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', required=True)
    args = parser.parse_args()
    publish_prepared(args.run)


if __name__ == '__main__':
    main()
