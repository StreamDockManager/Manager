"""Fetch and revalidate source-bound development artifacts for native qualification.

This does not install anything or establish publisher trust. Downloads are selected
from an authenticated delivery run, checked against the provider archive digest,
then checked against the product's native catalogue before a qualifier may use them.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import urllib.error
import urllib.parse
import urllib.request
import zipfile

from packaging_files import sha256_file

LIMIT = 512 * 1024 * 1024
PRODUCTS = {'manager': ('manager-development', 'catalog', 'stream-dock-manager'),
            'manager-cli': ('manager-cli-development', 'cli-catalog', 'stream-dock-manager-cli')}


# Calculate the file content digest used for integrity checks
def digest(path):
    return sha256_file(path)


# Check whether a path contains a filesystem link
def linked(path):
    return path.is_symlink() or (hasattr(path, 'is_junction') and path.is_junction())


# Require the expected regular file before reading trusted input
def regular(path, root):
    path, root = Path(path), Path(root)
    if not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Input must be a regular file inside its evidence directory')
    if any(linked(part) for part in (path, *path.parents)):
        raise ValueError('Qualification inputs cannot traverse symbolic links or junctions')
    return path


# Validate the requested repository and build identity
def identity(repository, commit, build, osname, product):
    if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository)
            or not re.fullmatch(r'[0-9a-f]{40}', commit)
            or not re.fullmatch(r'[1-9][0-9]*', str(build))
            or osname not in ('windows', 'linux') or product not in PRODUCTS):
        raise ValueError('Invalid selected repository, source, build or native product')
    return dict(repository=repository, commit=commit, build=str(build), os=osname,
                arch='amd64', product=product)


# Require a successful installer preparation run with matching provenance
def verify_run(info, selected):
    if (str(info.get('id')) != selected['build'] or info.get('head_sha') != selected['commit']
            or info.get('path') != '.github/workflows/delivery.yml'
            or not (info.get('event') in ('push', 'workflow_dispatch') and info.get('head_branch') == 'package-build')
            or info.get('status') != 'completed'
            or any(info.get(field, {}).get('full_name') != selected['repository']
                   for field in ('repository', 'head_repository'))):
        raise ValueError('Provider run does not match the selected completed development build')


# Verify the artifact identity and its declared integrity metadata
def verify_artifact(entry, selected):
    expected = f"{PRODUCTS[selected['product']][0]}-{selected['os']}-amd64"
    run = entry.get('workflow_run', {})
    if (entry.get('name') != expected or entry.get('expired') is not False
            or type(entry.get('id')) is not int or entry['id'] <= 0
            or str(run.get('id')) != selected['build'] or run.get('head_sha') != selected['commit']
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', str(entry.get('digest')))):
        raise ValueError('Provider artifact identity or digest is invalid')


# Validate archive member names, types, and size limits before extraction
def safe_members(archive):
    members, seen = archive.infolist(), set()
    if len(members) > 1000 or sum(item.file_size for item in members) > LIMIT:
        raise ValueError('Artifact archive exceeds qualification limits')
    for item in members:
        name = item.filename.rstrip('/') if item.is_dir() else item.filename
        parts = name.split('/')
        mode = item.external_attr >> 16
        if (item.orig_filename != item.filename or not name or '\\' in name
                or any(part in ('', '.', '..') or ':' in part or part.rstrip(' .') != part
                       or re.fullmatch(r'(?i)(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\..*)?', part)
                       for part in parts)
                or PurePosixPath(name).is_absolute() or name.casefold() in seen
                or stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR)
                or bool(item.flag_bits & 1)):
            raise ValueError('Unsafe or ambiguous artifact archive member')
        seen.add(name.casefold())
    files = {item.filename.casefold() for item in members if not item.is_dir()}
    if any('/'.join(item.filename.casefold().split('/')[:n]) in files
           for item in members for n in range(1, len(item.filename.split('/')))):
        raise ValueError('Archive file conflicts with a parent directory')
    return members


# Validate archive entries before extracting them into the owned destination
def extract(archive, destination):
    with zipfile.ZipFile(archive) as source:
        members = safe_members(source)
        destination.mkdir(exist_ok=False)
        for item in members:
            target = destination.joinpath(*PurePosixPath(item.filename).parts)
            if item.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(item) as original, target.open('xb') as output:
                    shutil.copyfileobj(original, output, 65536)


# Bind the files about to be installed back to the provider's actual ZIP
def verify_extracted(archive, destination):
    """Bind the files about to be installed back to the provider's actual ZIP."""
    with zipfile.ZipFile(archive) as source:
        members = [item for item in safe_members(source) if not item.is_dir()]
        actual = {p.relative_to(destination).as_posix() for p in destination.rglob('*') if p.is_file()}
        if actual != {item.filename for item in members}:
            raise ValueError('Extracted archive inventory differs')
        for item in members:
            path = regular(destination.joinpath(*PurePosixPath(item.filename).parts), destination)
            with source.open(item) as original:
                expected = hashlib.file_digest(original, 'sha256').hexdigest()
            if path.stat().st_size != item.file_size or digest(path) != expected:
                raise ValueError('Extracted bytes differ from the provider archive')


# Validate catalog identity and installer artifact metadata
def verify_catalog(root, selected, version=None):
    _, catalog_prefix, stem = PRODUCTS[selected['product']]
    paths = list(root.rglob(f"{catalog_prefix}-{selected['os']}-amd64.json"))
    if len(paths) != 1:
        raise ValueError('Expected exactly one selected native catalogue')
    catalog = json.loads(regular(paths[0], root).read_text(encoding='utf-8'))
    if (any(catalog.get(key) != selected[key] for key in ('repository', 'product', 'commit', 'build'))
            or catalog.get('schema_version') != 1
            or not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', str(catalog.get('version')))
            or (version is not None and catalog.get('version') != version)
            or catalog.get('channel') != 'development' or catalog.get('release_tag') != ''
            or catalog.get('packages') != [] or not isinstance(catalog.get('assets'), list)):
        raise ValueError('Native catalogue differs from the selected unreleased product')
    formats = {'.exe'} if selected['os'] == 'windows' else {'.deb', '.rpm'}
    if selected['product'] == 'manager-cli':
        formats.add('.zip')
    assets = {}
    for asset in catalog['assets']:
        extension = Path(str(asset.get('name', ''))).suffix
        expected = f"{stem}-{catalog['version']}-{selected['os']}-amd64{extension}"
        if (extension not in formats or extension in assets or asset.get('name') != expected
                or asset.get('os') != selected['os'] or asset.get('arch') != 'amd64'
                or asset.get('url') != '' or asset.get('signing') != 'unsigned'
                or type(asset.get('size')) is not int or not 0 < asset['size'] <= LIMIT
                or not re.fullmatch(r'[0-9a-f]{64}', str(asset.get('sha256')))):
            raise ValueError('Invalid native asset identity or checksum')
        matches = list(root.rglob(expected))
        if len(matches) != 1:
            raise ValueError('Native asset is missing or ambiguous')
        path = regular(matches[0], root)
        if path.stat().st_size != asset['size'] or digest(path) != asset['sha256']:
            raise ValueError('Native asset bytes differ from the catalogue')
        assets[extension] = path
    if set(assets) != formats:
        raise ValueError('Incomplete native product inventory')
    return catalog, assets


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Apply the download redirect policy before forwarding the request
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class GitHub:
    # Require a GitHub token and create an opener that does not follow redirects
    def __init__(self):
        self.token = os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
        if not self.token:
            raise ValueError('An authenticated read-only GitHub token is required')
        self.opener = urllib.request.build_opener(NoRedirect())

    # Request the fixed API resource with scoped authentication and redirect checks
    def request(self, path):
        request = urllib.request.Request('https://api.github.com/' + path, headers={
            'Authorization': f'Bearer {self.token}', 'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'StreamDock-installer-qualification'})
        return self.opener.open(request, timeout=90)

    # Decode the bounded API response as JSON
    def json(self, path):
        with self.request(path) as response:
            data = response.read(4 * 1024 * 1024 + 1)
        if len(data) > 4 * 1024 * 1024:
            raise ValueError('GitHub metadata response exceeds limits')
        return json.loads(data)

    # Download the selected artifact while checking its allowed source and integrity
    def download(self, path, destination):
        try:
            response = self.request(path)
        except urllib.error.HTTPError as error:
            if error.code not in (301, 302, 303, 307, 308):
                raise
            location = error.headers.get('Location', '')
            parsed = urllib.parse.urlsplit(location)
            error.close()
            if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.port not in (None, 443):
                raise ValueError('Artifact redirect must use HTTPS') from None
            # A new request deliberately carries no API token to blob storage
            # Refuse additional redirects, including an HTTPS-to-HTTP downgrade
            response = self.opener.open(urllib.request.Request(location), timeout=90)
        size = 0
        with response, destination.open('xb') as output:
            while chunk := response.read(65536):
                size += len(chunk)
                if size > LIMIT:
                    raise ValueError('Provider archive exceeds download limit')
                output.write(chunk)


# Write the validated input metadata for subsequent release steps
def save(path, value):
    with path.open('x', encoding='utf-8') as output:
        json.dump(value, output, indent=2)
        output.write('\n')


# Retrieve and validate the selected installer preparation inputs
def fetch(output, selected, api):
    if any(linked(part) for part in (output, *output.parents)):
        raise ValueError('Output cannot traverse links or junctions')
    output.mkdir(parents=True, exist_ok=False)
    base = f"repos/{selected['repository']}"
    info = api.json(f"{base}/actions/runs/{selected['build']}")
    save(output / 'provider-run.json', info)
    verify_run(info, selected)
    entries = []
    for page in range(1, 101):
        data = api.json(f"{base}/actions/runs/{selected['build']}/artifacts?per_page=100&page={page}")
        save(output / f'provider-artifacts-{page}.json', data)
        entries.extend(data['artifacts'])
        if len(entries) == data['total_count']:
            break
        if len(entries) > data['total_count'] or not data['artifacts']:
            raise ValueError('Inconsistent artifact pagination')
    else:
        raise ValueError('Artifact pagination exceeds limit')
    expected = f"{PRODUCTS[selected['product']][0]}-{selected['os']}-amd64"
    entries = [entry for entry in entries if entry.get('name') == expected]
    if len(entries) != 1:
        raise ValueError('Native provider artifact is missing or ambiguous')
    entry = entries[0]
    verify_artifact(entry, selected)
    save(output / 'provider-artifact.json', entry)
    archive = output / 'provider-artifact.zip'
    api.download(f"{base}/actions/artifacts/{entry['id']}/zip", archive)
    if 'sha256:' + digest(archive) != entry['digest']:
        raise ValueError('Provider artifact archive digest differs')
    extract(archive, output / 'artifact')
    verify_catalog(output / 'artifact', selected)
    sources = {'VERSION': 'VERSION', 'LICENSE': 'LICENSE', 'THIRD_PARTY_NOTICES.md': 'THIRD_PARTY_NOTICES.md',
               'INSTALL-INSTRUCTIONS.md': f"packaging/install-instructions/{selected['os']}/INSTALL-INSTRUCTIONS.md"}
    if selected['os'] == 'linux':
        sources['60-stream-dock-manager.rules'] = 'packaging/linux/60-stream-dock-manager.rules'
    (output / 'source').mkdir()
    for name, path in sources.items():
        data = api.json(f"{base}/contents/{path}?ref={selected['commit']}")
        if data.get('type') != 'file' or data.get('path') != path or data.get('encoding') != 'base64':
            raise ValueError('Unexpected pinned source file response')
        content = base64.b64decode(''.join(data['content'].split()), validate=True)
        if len(content) != data.get('size') or len(content) > 1024 * 1024:
            raise ValueError('Pinned source file exceeds limits or differs in size')
        (output / 'source' / name).write_bytes(content)
    files = {path.relative_to(output).as_posix(): {'size': path.stat().st_size, 'sha256': digest(path)}
             for path in sorted(output.rglob('*')) if path.is_file()}
    save(output / 'inputs.json', {'schema_version': 1, 'identity': selected,
                                'source_files': sources, 'files': files})
    return load_inputs(output, repository=selected['repository'], commit=selected['commit'],
                       build=selected['build'], osname=selected['os'], product=selected['product'])


# Load and verify prepared installer inputs from the selected directory
def load_inputs(directory, *, repository, commit, build, osname, product):
    root = Path(directory).absolute()
    selected = identity(repository, commit, build, osname, product)
    receipt = json.loads(regular(root / 'inputs.json', root).read_text(encoding='utf-8'))
    if receipt.get('schema_version') != 1 or receipt.get('identity') != selected:
        raise ValueError('Qualification receipt identity differs')
    files = receipt.get('files', {})
    actual = {p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()}
    if actual != {*files, 'inputs.json'}:
        raise ValueError('Qualification receipt inventory differs')
    for name, record in files.items():
        path = regular(root / name, root)
        if path.stat().st_size != record['size'] or digest(path) != record['sha256']:
            raise ValueError('Qualification receipt byte verification failed')
    run = json.loads((root / 'provider-run.json').read_text(encoding='utf-8'))
    entry = json.loads((root / 'provider-artifact.json').read_text(encoding='utf-8'))
    verify_run(run, selected)
    verify_artifact(entry, selected)
    if 'sha256:' + digest(root / 'provider-artifact.zip') != entry['digest']:
        raise ValueError('Provider archive does not match its authenticated digest')
    verify_extracted(root / 'provider-artifact.zip', root / 'artifact')
    expected_sources = {'VERSION', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'INSTALL-INSTRUCTIONS.md'}
    if osname == 'linux':
        expected_sources.add('60-stream-dock-manager.rules')
    if set(receipt.get('source_files', {})) != expected_sources:
        raise ValueError('Pinned source inventory is incomplete')
    source_files = {name: regular(root / 'source' / name, root) for name in expected_sources}
    version = source_files['VERSION'].read_text(encoding='utf-8').strip()
    catalog, assets = verify_catalog(root / 'artifact', selected, version=version)
    return {'catalog': catalog, 'assets': assets, 'source_files': source_files, 'receipt': receipt}


# Parse options and run the installer inputs workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--build', required=True)
    parser.add_argument('--os', required=True, choices=['linux', 'windows'])
    parser.add_argument('--product', required=True, choices=list(PRODUCTS))
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    selected = identity(args.repository, args.commit, args.build, args.os, args.product)
    fetch(args.output.absolute(), selected, GitHub())
    print('Verified native qualification inputs; no installer was executed.')


if __name__ == '__main__':
    main()
