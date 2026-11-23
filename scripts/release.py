#!/usr/bin/env python3
"""Independent product version/tag/catalogue and immutable draft publication tooling."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib

from packaging_files import sha256_file

from native_targets import INSTALLER_VARIANTS, PORTABLE_VARIANTS

ROOT = Path(__file__).resolve().parent.parent
PRODUCT = 'manager'
CLI_PRODUCT = 'manager-cli'
NOTICES = 'THIRD_PARTY_NOTICES.md'
MAX_NOTICES = 4 * 1024**2
COMPATIBILITY_NOTICE = 'Initial public release: build and package integrity verified, but vendor compatibility is partial and exhaustive native, installer and physical-device qualification is not complete.'
CATALOG_RELEASE_NOTES = COMPATIBILITY_NOTICE + ' Unsigned and unnotarized release. Checksums verify download integrity, not publisher identity or correspondence to source.'
VARIANTS = set(INSTALLER_VARIANTS)
CLI_VARIANTS = {*INSTALLER_VARIANTS, *PORTABLE_VARIANTS}
IDENTITY_FIELDS = ['schema_version', 'repository', 'version', 'channel', 'commit', 'release_tag', 'build']
TARGET_PROFILES = frozenset(('standard', 'with-intel'))



# Run the selected command and propagate failure to the calling workflow
def run(args, capture=False):
    environment = {**os.environ, 'GH_HOST': 'github.com'}
    return subprocess.check_output(args, cwd=ROOT, env=environment, text=True).strip() if capture else subprocess.check_call(args, cwd=ROOT, env=environment)



# Calculate the file content digest used for integrity checks
def file_digest(path):
    return sha256_file(path)


# Compare files by content rather than filename alone
def same_file_bytes(left, right):
    if left.stat().st_size != right.stat().st_size:
        return False
    with left.open('rb') as a, right.open('rb') as b:
        while True:
            chunk = a.read(256 * 1024)
            if chunk != b.read(256 * 1024):
                return False
            if not chunk:
                return True


# Read the current product release version
def version():
    value = os.environ.get('PREPARED_RELEASE_VERSION') or (ROOT / 'VERSION').read_text().strip()
    if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', value):
        raise ValueError('VERSION must be a canonical stable semantic version')
    return value


# Validate the coordinated product version declarations
def versions():
    value = version()
    for relative in ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']:
        path = ROOT / relative
        data = json.loads(path.read_text())
        if data['version'] != value or relative == 'package-lock.json' and data['packages']['']['version'] != value:
            raise ValueError(f'{relative} differs from VERSION; align the project manifests and regenerate lockfiles before preparing a release')
    path = ROOT / 'Cargo.toml'
    source = path.read_text()
    current = tomllib.loads(source)['workspace']['package']['version']
    if current != value:
        raise ValueError('Cargo workspace version differs from VERSION; align the project manifests and regenerate lockfiles before preparing a release')
    local_names = {'xtask'}
    for path in [*ROOT.glob('crates/*/Cargo.toml'), *ROOT.glob('hardware/*/Cargo.toml'), ROOT / 'src-tauri/Cargo.toml']:
        local_names.add(tomllib.loads(path.read_text())['package']['name'])
    path = ROOT / 'Cargo.lock'
    entries = tomllib.loads(path.read_text())['package']
    for name in local_names:
        matches = [entry for entry in entries if entry['name'] == name and 'source' not in entry]
        if len(matches) != 1 or matches[0]['version'] != value:
            raise ValueError(f'{name} lockfile version differs from VERSION; align the project manifests and regenerate lockfiles before preparing a release')


# Build release inputs without creating remote tags or releases
def build_only():
    return os.environ.get('BUILD_ONLY') == 'true'


# Restrict publication to an explicit main dispatch in the canonical product repository
def require_publication_context():
    if build_only():
        raise ValueError('Build-only artifacts cannot create tags or publish releases')
    if (os.environ.get('GITHUB_REPOSITORY') != 'StreamDockManager/Manager'
            or os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or os.environ.get('GITHUB_REF_NAME') != 'main'):
        raise ValueError('Publication requires an explicit main dispatch in StreamDockManager/Manager')


# Verify that the selected release repository matches the expected identity
def verify_repository(repository):
    info = json.loads(run(['gh', 'api', f'repos/{repository}'], True))
    if (not isinstance(info, dict) or not isinstance(info.get('full_name'), str) or info['full_name'].lower() != repository.lower()):
        raise ValueError('Release repository identity could not be verified')


# Render release notes from the validated release metadata
def release_notes(catalog):
    repository, commit, build = catalog['repository'], catalog['commit'], catalog['build']
    base = f'https://github.com/{repository}'
    return (
        COMPATIBILITY_NOTICE + '\n\n'
        '**Unsigned and unnotarized release.** These installers and CLI archives have no code-signing or detached signatures. '
        'Windows and macOS may warn about or block them.\n\n'
        'The desktop installers do not include the CLI. Download a separate `stream-dock-manager-cli` archive '
        'only if you need command-line access; the desktop app works independently.\n\n'
        '`SHA256SUMS` checks downloaded bytes for integrity; it does not authenticate the publisher '
        'or prove that a binary matches the source. The source and build links below are provided for inspection.\n\n'
        f'- Source commit: [{commit}]({base}/commit/{commit})\n'
        f'- Build run: [{build}]({base}/actions/runs/{build})\n'
        f'- Native build workflow: [build-installers.yml]({base}/blob/{commit}/.github/workflows/build-installers.yml)\n'
        f'- CLI build workflow: [build-cli.yml]({base}/blob/{commit}/.github/workflows/build-cli.yml)\n'
        f'- [Third-party notices]({base}/blob/{commit}/{NOTICES}) (also available as a release asset)\n'
        f'- Preparation workflow: [prepare-release.yml]({base}/blob/{commit}/.github/workflows/prepare-release.yml)\n\n'
        'Installation instructions are included in each OS package and can be read before downloading:\n'
        + ''.join(f'- [{osname.title()} installation instructions]({base}/blob/{commit}/packaging/install-instructions/{osname}/INSTALL-INSTRUCTIONS.md)\n'
                  for osname in ['macos', 'windows', 'linux'])
    )


# Attach the generated notes to the selected release payload
def release_with_notes(args, catalog):
    # A file preserves literal newlines and avoids shell interpolation. Close it
    # before invoking gh so this also works on Windows
    with tempfile.TemporaryDirectory() as temp:
        notes = Path(temp) / 'release-notes.md'
        notes.write_text(release_notes(catalog), encoding='utf-8')
        run([*args, '--notes-file', str(notes)])


# Construct the release plan for the selected source version and targets
def plan():
    versions()
    value = version()
    sha = run(['git', 'rev-parse', 'HEAD'], True)
    if build_only():
        with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
            stream.write(f'tag=\nchannel=development\ncommit={sha}\n')
        return
    if os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch' or os.environ.get('GITHUB_REF_NAME') != 'main':
        raise ValueError('Release preparation must be dispatched from main')
    require_publication_context()
    with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
        stream.write(f'tag=v{value}\nchannel=stable\ncommit={sha}\n')


# Build the Manager installer catalog from validated package metadata
def manager_catalog(directory, osname, arch):
    value = version()
    repo = os.environ['GITHUB_REPOSITORY']
    tag = '' if build_only() else os.environ['RELEASE_TAG']
    assets = []
    for path in sorted(directory.rglob('*')):
        if path.suffix.lower() not in ['.dmg', '.exe', '.deb', '.rpm']:
            continue
        # Tauri names can contain spaces. Use stable outer filenames that GitHub
        # will preserve and the Manager can validate without URL normalization
        name = f'stream-dock-manager-{value}-{osname}-{arch}{path.suffix.lower()}'
        if any(asset['name'] == name for asset in assets):
            raise ValueError(f'Duplicate native installer variant: {name}')
        if path.name != name:
            target = path.with_name(name)
            if target.exists():
                raise ValueError(f'Native installer destination already exists: {name}')
            path.rename(target)
            path = target
        assets.append({'name': path.name, 'url': '' if build_only() else f'https://github.com/{repo}/releases/download/{tag}/{path.name}',
                       'sha256': file_digest(path), 'size': path.stat().st_size,
                       # Every release is deliberately unsigned and unnotarized
                       'os': osname, 'arch': arch, 'signing': 'unsigned'})
    if not assets:
        raise ValueError('No native installers produced')
    catalog = {'schema_version': 1, 'product': PRODUCT, 'repository': repo, 'version': value, 'channel': 'development' if build_only() else 'stable',
               'build': os.environ['GITHUB_RUN_ID'], 'commit': os.environ['RELEASE_COMMIT'], 'published_at': datetime.now(timezone.utc).isoformat(),
               'release_tag': tag, 'assets': assets, 'packages': []}
    catalog['release_notes'] = ('Development build; not published or release-qualified.\n\n' if build_only() else '') + CATALOG_RELEASE_NOTES
    (directory / f'catalog-{osname}-{arch}.json').write_text(json.dumps(catalog, indent=4) + '\n')


# Validate installer catalog fields before release assembly
def validate_catalog_metadata(catalog, *, complete, product=PRODUCT, target_profile=None):
    if target_profile is not None and target_profile not in TARGET_PROFILES:
        raise ValueError('Unknown selected release target profile')
    # Historical preparations had no field and always required the Intel rows.
    # Only native input fragments can inherit the selection from the workflow.
    profile = catalog.get('target_profile', target_profile if not complete and target_profile else 'with-intel')
    if not isinstance(profile, str) or profile not in TARGET_PROFILES:
        raise ValueError('Unknown release target profile')
    if target_profile is not None and profile != target_profile:
        raise ValueError('Catalogue target profile differs from the selected preparation')
    expected_variants = CLI_VARIANTS if product == CLI_PRODUCT else VARIANTS
    if profile == 'standard':
        expected_variants = {row for row in expected_variants if row[:2] != ('macos', 'amd64')}
    if (product not in (PRODUCT, CLI_PRODUCT) or catalog['repository'] != os.environ['GITHUB_REPOSITORY']
            or catalog['product'] != product or catalog['schema_version'] != 1):
        raise ValueError('Wrong publication product/repository')
    if catalog['version'] != version() or catalog['release_tag'] != f'v{version()}' or catalog['channel'] != 'stable':
        raise ValueError('Publication identity differs from checked-in VERSION')
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', catalog['commit']) or not re.fullmatch(r'[0-9]+', catalog['build']):
        raise ValueError('Invalid publication commit/build identity')
    if datetime.fromisoformat(catalog['published_at']).tzinfo is None:
        raise ValueError('Publication timestamp must include a timezone')
    if os.environ.get('RELEASE_COMMIT') and catalog['commit'] != os.environ['RELEASE_COMMIT']:
        raise ValueError('Publication commit differs from the selected source')
    if os.environ.get('RELEASE_BUILD') and catalog['build'] != os.environ['RELEASE_BUILD']:
        raise ValueError('Publication build differs from the selected workflow run')
    assets = catalog['assets']
    if not isinstance(assets, list) or not assets or catalog.get('packages'):
        raise ValueError('Product catalogues contain only their native downloads')
    names, variants = set(), set()
    for asset in assets:
        name = asset['name']
        if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name) or name in names:
            raise ValueError('Invalid or duplicate installer filename')
        variant = (asset['os'], asset['arch'], Path(name).suffix.lower())
        if variant not in expected_variants or variant in variants:
            raise ValueError('Invalid or duplicate native installer variant' if product == PRODUCT else 'Invalid or duplicate native CLI variant')
        if product == CLI_PRODUCT and name != f'stream-dock-manager-cli-{catalog["version"]}-{asset["os"]}-{asset["arch"]}{variant[2]}':
            raise ValueError('CLI filename differs from its version/platform')
        if asset['url'] != f'https://github.com/{catalog["repository"]}/releases/download/{catalog["release_tag"]}/{name}':
            raise ValueError('Installer URL differs from the release repository/tag/name')
        if (type(asset['size']) is not int or not 0 < asset['size'] <= 2 * 1024 * 1024 * 1024
                or not isinstance(asset['sha256'], str) or not re.fullmatch(r'[0-9a-f]{64}', asset['sha256'])):
            raise ValueError('Invalid native installer checksum/size')
        if asset.get('signing') != 'unsigned':
            raise ValueError('Manager release installers must be marked unsigned')
        names.add(name)
        variants.add(variant)
    if complete and variants != expected_variants:
        raise ValueError('Release must include exactly the supported native installer variants' if product == PRODUCT else 'Release must include exactly the supported native CLI variants')


# Require source, version, repository, and artifact identities to agree
def validate_matching_identity(catalogs):
    first = catalogs[0]
    for catalog in catalogs[1:]:
        for field in IDENTITY_FIELDS:
            if catalog[field] != first[field]:
                raise ValueError(f'Native catalogue mismatch: {field}')
        if catalog.get('target_profile') != first.get('target_profile'):
            raise ValueError('Native catalogue mismatch: target_profile')


# Require a regular file contained in the selected release directory
def regular_file_in(path, root):
    if not path.is_file() or path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Native release input must be a regular file inside the artifact directory')
    if any(parent.is_symlink() for parent in path.parents if parent != root and root in parent.parents):
        raise ValueError('Native release input cannot traverse symlink directories')


# Validate the standalone notices before copying or publishing them
def notices_digest(path, root):
    if not path.exists() and not path.is_symlink():
        raise ValueError(f'Missing {NOTICES}; run Prepare Release with standalone notices support')
    regular_file_in(path, root)
    if not 0 < path.stat().st_size <= MAX_NOTICES:
        raise ValueError(f'{NOTICES} has an invalid size')
    return file_digest(path)


# Assemble verified installer inputs into the publication bundle
def assemble(directory):
    products = [(PRODUCT, 'catalog-*.json', 'catalog.json'),
                (CLI_PRODUCT, 'cli-catalog-*.json', 'cli-catalog.json')]
    combined, native = {}, []
    target_profile = os.environ.get('RELEASE_TARGET_PROFILE')
    expected_profile = target_profile if target_profile is not None else 'with-intel'
    for product, pattern, filename in products:
        catalogs = []
        for path in sorted(directory.rglob(pattern)):
            regular_file_in(path, directory)
            catalog = json.loads(path.read_text())
            # Validate untrusted names and provenance before any output writes
            validate_catalog_metadata(catalog, complete=False, product=product, target_profile=expected_profile)
            catalogs.append(catalog)
        if not catalogs:
            raise ValueError(f'Missing native catalogues for {product}')
        validate_matching_identity(catalogs)
        first = dict(catalogs[0])
        first['assets'] = [asset for catalog in catalogs for asset in catalog['assets']]
        first['packages'] = []
        if target_profile is not None:
            first['target_profile'] = target_profile
        validate_catalog_metadata(first, complete=True, product=product, target_profile=expected_profile)
        first['release_notes'] = CATALOG_RELEASE_NOTES
        combined[filename] = first
        native.extend(catalogs)
    validate_matching_identity(list(combined.values()))
    # Reuse persisted native metadata for byte-identical publication retries
    # Both products share the same build timestamp in their final catalogues
    timestamp = max(datetime.fromisoformat(c['published_at']) for c in native).isoformat()
    for catalog in combined.values():
        catalog['published_at'] = timestamp
    assets = [asset for catalog in combined.values() for asset in catalog['assets']]
    names = [asset['name'] for asset in assets]
    if len(set(names)) != len(names) or set(names) & {*combined, NOTICES, 'SHA256SUMS'}:
        raise ValueError('Duplicate release filename across product catalogues')
    output = directory / 'publish'
    if output.is_symlink() or output.exists() and not output.is_dir():
        raise ValueError('Publication output must be a regular directory')
    inputs = []
    for asset in assets:
        paths = [p for p in directory.rglob(asset['name']) if output not in p.parents]
        if len(paths) != 1:
            raise ValueError(f'Missing/ambiguous asset: {asset["name"]}')
        regular_file_in(paths[0], directory)
        with paths[0].open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if paths[0].stat().st_size != asset['size'] or digest != asset['sha256']:
            raise ValueError('Native asset checksum mismatch')
        inputs.append((asset, paths[0]))
    source_notices = ROOT / NOTICES
    notice_digest = notices_digest(source_notices, ROOT)
    for name in [*names, *combined, NOTICES, 'SHA256SUMS']:
        path = output / name
        if path.is_symlink() or path.exists() and not path.is_file():
            raise ValueError('Publication output cannot replace a symlink or non-file')
    output.mkdir(exist_ok=True)
    sums = []
    for asset, source in inputs:
        shutil.copyfile(source, output / asset['name'])
        sums.append(f'{asset["sha256"]}  {asset["name"]}')
    for filename, catalog in combined.items():
        path = output / filename
        path.write_text(json.dumps(catalog, indent=4) + '\n')
        sums.append(file_digest(path) + '  ' + filename)
    shutil.copyfile(source_notices, output / NOTICES)
    sums.append(f'{notice_digest}  {NOTICES}')
    (output / 'SHA256SUMS').write_text('\n'.join(sums) + '\n')
    return output


# Verify publication metadata and asset digests before remote writes
def verify_publication(directory):
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError('Publication input must be a regular directory')
    # Restored publication artifacts bypass assembly. Inspect every entry first,
    # then validate both catalogues and their shared identity before any upload
    for path in directory.iterdir():
        regular_file_in(path, directory)
    catalogs, expected = [], {NOTICES: notices_digest(directory / NOTICES, directory)}
    for product, filename in [(PRODUCT, 'catalog.json'), (CLI_PRODUCT, 'cli-catalog.json')]:
        path = directory / filename
        if not path.is_file():
            raise ValueError(f'Missing publication catalogue: {filename}')
        catalog = json.loads(path.read_text())
        validate_catalog_metadata(catalog, complete=True, product=product,
                                  target_profile=os.environ.get('RELEASE_TARGET_PROFILE'))
        if catalog.get('release_notes') != CATALOG_RELEASE_NOTES:
            raise ValueError('Publication notes must contain the unsigned release warning; reassemble the original native artifacts')
        catalogs.append(catalog)
        expected[filename] = file_digest(path)
    validate_matching_identity(catalogs)
    for catalog in catalogs:
        for asset in catalog['assets']:
            name = asset['name']
            if name in expected:
                raise ValueError('Duplicate release filename across product catalogues')
            path = directory / name
            regular_file_in(path, directory)
            with path.open('rb') as stream:
                digest = hashlib.file_digest(stream, 'sha256').hexdigest()
            if digest != asset['sha256'] or path.stat().st_size != asset['size']:
                raise ValueError('Installer or CLI checksum/size mismatch')
            expected[name] = digest
    checksums = {}
    for line in (directory / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        if name in checksums:
            raise ValueError('Duplicate checksum entry')
        checksums[name] = digest
    present = {path.name for path in directory.iterdir()}
    if checksums != expected or present != {*expected, 'SHA256SUMS'}:
        raise ValueError('Release inventory/checksums are incomplete or contain unexpected files')
    return catalogs[0]


# Read the existing remote release without changing it
def remote_release(repository, tag):
    # A failed authenticated listing is an error, never an empty baseline
    pages = json.loads(run(['gh', 'api', '--paginate', '--slurp', f'repos/{repository}/releases?per_page=100'], True))
    if not isinstance(pages, list) or not pages or any(not isinstance(page, list) for page in pages):
        raise ValueError('Unexpected paginated release response')
    matches = []
    for page in pages:
        for release in page:
            if (not isinstance(release, dict) or not isinstance(release.get('tag_name'), str)
                    or type(release.get('id')) is not int or release['id'] <= 0
                    or not isinstance(release.get('draft'), bool) or not isinstance(release.get('prerelease'), bool)
                    or not isinstance(release.get('target_commitish'), str)
                    or not isinstance(release.get('assets'), list)
                    or any(not isinstance(asset, dict) or not isinstance(asset.get('name'), str) for asset in release['assets'])):
                raise ValueError('Unexpected release record')
            if release['tag_name'] == tag:
                matches.append(release)
    if len(matches) > 1:
        raise ValueError('Duplicate release tag in remote history')
    return matches[0] if matches else None


# Require the remote release tag to identify the prepared source commit
def verify_remote_tag(repository, tag, commit):
    ref = json.loads(run(['gh', 'api', f'repos/{repository}/git/ref/tags/{tag}'], True))
    if not isinstance(ref, dict) or ref.get('ref') != f'refs/tags/{tag}':
        raise ValueError('Remote release tag identity mismatch')
    obj, seen = ref.get('object'), set()
    for _ in range(16):
        if not isinstance(obj, dict) or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', str(obj.get('sha', ''))):
            raise ValueError('Invalid remote release tag object')
        sha = obj['sha']
        if obj.get('type') == 'commit':
            if sha != commit:
                raise ValueError('Remote release tag points to a different source commit')
            return
        if obj.get('type') != 'tag' or sha in seen:
            raise ValueError('Invalid or cyclic annotated release tag')
        seen.add(sha)
        annotated = json.loads(run(['gh', 'api', f'repos/{repository}/git/tags/{sha}'], True))
        obj = annotated.get('object') if isinstance(annotated, dict) else None
    raise ValueError('Annotated release tag nesting exceeds limit')


# Download remote release assets for byte-for-byte verification
def download_release(repository, tag, directory):
    run(['gh', 'release', 'download', tag, '--repo', repository, '--dir', str(directory)])


# Capture the created draft directly instead of relying on immediate release-list visibility
def create_draft(repository, tag, catalog):
    payload = {'tag_name': tag, 'target_commitish': catalog['commit'], 'draft': True,
               'prerelease': False, 'name': f'{PRODUCT.title()} {catalog["version"]}',
               'body': release_notes(catalog)}
    with tempfile.TemporaryDirectory() as temporary:
        request = Path(temporary) / 'draft.json'
        request.write_text(json.dumps(payload), encoding='utf-8')
        created = json.loads(run(['gh', 'api', '--method', 'POST',
                                  f'repos/{repository}/releases', '--input', str(request)], True))
    if (not isinstance(created, dict) or type(created.get('id')) is not int or created['id'] <= 0
            or created.get('tag_name') != tag or created.get('target_commitish') != catalog['commit']
            or created.get('draft') is not True or created.get('prerelease') is not False
            or created.get('assets') != []):
        raise ValueError('Created draft response does not match the selected tag, source and empty draft identity')
    # The authenticated creation response is authoritative even before GET/list propagation
    # Publication still revalidates this exact ID and the remote tag after asset verification
    return created


# Publish only verified staged assets while preserving immutable release identity
def publish(directory):
    require_publication_context()
    catalog = verify_publication(directory)
    tag, repository = catalog['release_tag'], catalog['repository']
    verify_repository(repository)
    existing = remote_release(repository, tag)
    verify_remote_tag(repository, tag, catalog['commit'])
    if existing and existing['target_commitish'] != catalog['commit']:
        raise ValueError('Release target differs from the selected source commit')
    if existing and existing['prerelease']:
        raise ValueError('Stable release tag is already a prerelease')
    if existing and not existing['draft']:
        # Published releases are immutable. Verify the complete remote inventory
        # against its catalogue, including files a truncated checksum list omits
        with tempfile.TemporaryDirectory() as temp:
            download_release(repository, tag, temp)
            remote = verify_publication(Path(temp))
            for field in ['product', 'repository', 'version', 'commit', 'build', 'channel', 'release_tag', 'target_profile']:
                if remote.get(field) != catalog.get(field):
                    raise ValueError('Published release belongs to a different build; refusing overwrite')
            if any(not same_file_bytes(Path(temp) / path.name, path) for path in directory.iterdir()):
                raise ValueError('Published release bytes differ from prepared artifacts; refusing overwrite')
        print('Published release verified; nothing overwritten')
        return
    if existing is None:
        existing = create_draft(repository, tag, catalog)
    # Validate every staged asset before uploading anything. Path sorting differs
    # across platforms and must not determine when a conflicting draft is rejected
    with tempfile.TemporaryDirectory() as temp:
        if existing and existing['assets']:
            download_release(repository, tag, temp)
        if not {path.name for path in Path(temp).iterdir()} <= {path.name for path in directory.iterdir()}:
            raise ValueError('Draft contains unexpected assets')
        for old in Path(temp).iterdir():
            if not same_file_bytes(old, directory / old.name):
                raise ValueError(f'Draft asset differs: {old.name}; refusing overwrite')
        for path in sorted(directory.iterdir()):
            if not (Path(temp) / path.name).exists():
                run(['gh', 'release', 'upload', tag, str(path), '--repo', repository])
    # Verify uploaded bytes before making a draft visible
    with tempfile.TemporaryDirectory() as temp:
        download_release(repository, tag, temp)
        verify_publication(Path(temp))
        if any(not same_file_bytes(Path(temp) / path.name, path) for path in directory.iterdir()):
            raise ValueError('Uploaded release differs from the prepared artifacts')
    verify_remote_tag(repository, tag, catalog['commit'])
    current = remote_release(repository, tag)
    if not current or current['id'] != existing['id'] or current['target_commitish'] != catalog['commit'] or current['prerelease']:
        raise ValueError('Release identity changed during publication')
    if not current['draft']:
        print('Published release verified; nothing overwritten')
        return
    release_with_notes(['gh', 'release', 'edit', tag, '--repo', repository, '--draft=false'], catalog)


# Parse options and run the release workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['version', 'plan', 'manager-catalog', 'assemble', 'verify', 'publish'])
    parser.add_argument('--dir', type=Path, default=ROOT / 'artifacts')
    parser.add_argument('--os')
    parser.add_argument('--arch')
    parser.add_argument('--run')
    args = parser.parse_args()
    if args.command == 'version':
        versions()
    elif args.command == 'plan':
        plan()
    elif args.command == 'manager-catalog':
        manager_catalog(args.dir, args.os, args.arch)
    elif args.command == 'assemble':
        assemble(args.dir)
    elif args.command == 'verify':
        verify_publication(args.dir)
    elif args.command == 'publish':
        publish(args.dir)


if __name__ == '__main__':
    main()
