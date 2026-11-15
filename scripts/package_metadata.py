#!/usr/bin/env python3
"""Validated shared metadata for desktop and CLI installers; never rewrite tracked manifests."""
import argparse
from datetime import date
import json
import os
from pathlib import Path
import re
import tomllib
from urllib.parse import urlsplit
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
VARIABLE = 'STREAM_DOCK_MANAGER_PACKAGE_METADATA'
FIELDS = {
    'package': {'publisher', 'website', 'license', 'last_updated', 'copyright'},
    'desktop': {'name', 'summary', 'description', 'developer_id'},
    'cli': {'summary', 'description', 'license'},
    'linux': {'section', 'priority', 'rpm_release'},
}


# Resolve the package metadata configuration within the allowed source tree
def config_path(env=None):
    env = os.environ if env is None else env
    path = Path(env.get(VARIABLE) or 'packaging/metadata.toml')
    if not path.is_absolute():
        path = ROOT / path
    path = path.resolve()
    if not path.is_relative_to(ROOT) or not path.is_file():
        raise ValueError(f'{VARIABLE} must select a file within the Manager checkout')
    return path


# Parse and validate the supported SPDX license expression
def validate_license_expression(value):
    # Validate SPDX expression grammar, not membership in a license-ID registry.
    tokens = re.findall(r'[A-Za-z0-9][A-Za-z0-9.+-]*|[()]', value)
    if ''.join(tokens) != re.sub(r'\s+', '', value):
        raise ValueError('License must use SPDX expression syntax')
    position = 0
    # Parse the next license-expression operator group
    def expression():
        nonlocal position
        atom()
        while position < len(tokens) and tokens[position] in ('AND', 'OR'):
            position += 1
            atom()
    # Parse one license identifier or parenthesized expression
    def atom():
        nonlocal position
        if position >= len(tokens):
            raise ValueError('Incomplete SPDX expression')
        token = tokens[position]
        position += 1
        if token == '(':
            expression()
            if position >= len(tokens) or tokens[position] != ')':
                raise ValueError('Unbalanced SPDX expression')
            position += 1
        elif token in ('AND', 'OR', 'WITH', ')'):
            raise ValueError('Invalid SPDX expression')
        if position < len(tokens) and tokens[position] == 'WITH':
            if token == '(':
                raise ValueError('SPDX WITH must follow a single license identifier')
            position += 1
            if position >= len(tokens) or tokens[position] in ('AND', 'OR', 'WITH', '(', ')'):
                raise ValueError('Missing SPDX exception identifier')
            position += 1
    expression()
    if position != len(tokens):
        raise ValueError('Invalid SPDX expression')


# Validate the selected metadata against the module’s supported contract
def validate(data):
    if set(data) != set(FIELDS):
        raise ValueError('Package metadata requires package, desktop, cli and linux tables')
    for section, keys in FIELDS.items():
        table = data[section]
        if not isinstance(table, dict) or set(table) != keys:
            raise ValueError(f'Invalid or missing fields in {section} metadata')
        for key, value in table.items():
            if (not isinstance(value, str) or len(value) > 4096 or value != value.strip()
                    or (not value and key != 'last_updated')
                    or any(ord(c) < 32 or ord(c) == 127 for c in value)):
                raise ValueError(f'{section}.{key} must be a bounded single-line string')
    url = urlsplit(data['package']['website'])
    if (url.scheme != 'https' or not url.hostname or url.username or url.password
            or any(c.isspace() for c in data['package']['website'])):
        raise ValueError('package.website must be an HTTPS URL without credentials')
    if data['package']['last_updated']:
        value = data['package']['last_updated']
        if not re.fullmatch(r'\d{4}-\d{2}-\d{2}', value):
            raise ValueError('package.last_updated must be YYYY-MM-DD or empty')
        date.fromisoformat(value)
    for section in ('package', 'cli'):
        validate_license_expression(data[section]['license'])
    if not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)+', data['desktop']['developer_id']):
        raise ValueError('desktop.developer_id must be a reverse-DNS identifier')
    if not re.fullmatch(r'[a-z0-9+./-]+', data['linux']['section']):
        raise ValueError('Invalid Debian section')
    if data['linux']['priority'] not in ('required', 'important', 'standard', 'optional', 'extra'):
        raise ValueError('Invalid Debian priority')
    if not re.fullmatch(r'[A-Za-z0-9._+]+', data['linux']['rpm_release']):
        raise ValueError('Invalid RPM release')
    return data


# Load and validate package metadata configuration
def load(env=None):
    with config_path(env).open('rb') as source:
        return validate(tomllib.load(source))


# Render AppStream metadata with escaped XML values
def appstream(data, version):
    component = ET.Element('component', {'type': 'desktop-application'})
    # Validate one package metadata value before rendering it
    def item(name, value, parent=component, **attributes):
        node = ET.SubElement(parent, name, attributes)
        node.text = value
        return node
    item('id', 'app.streamdockmanager.manager')
    item('metadata_license', 'CC0-1.0')
    item('project_license', data['package']['license'])
    item('name', data['desktop']['name'])
    item('summary', data['desktop']['summary'])
    item('p', data['desktop']['description'], ET.SubElement(component, 'description'))
    developer = ET.SubElement(component, 'developer', {'id': data['desktop']['developer_id']})
    item('name', data['package']['publisher'], developer)
    item('url', data['package']['website'], type='homepage')
    item('launchable', 'app.streamdockmanager.manager.desktop', type='desktop-id')
    item('binary', 'stream-dock-manager', ET.SubElement(component, 'provides'))
    item('category', 'Utility', ET.SubElement(component, 'categories'))
    if data['package']['last_updated']:
        ET.SubElement(ET.SubElement(component, 'releases'), 'release', {
            'version': version, 'date': data['package']['last_updated']})
    ET.indent(component)
    return ET.tostring(component, encoding='unicode', xml_declaration=True) + '\n'


# Write the generated desktop and AppStream metadata documents
def write_documents(directory, data, product, version):
    directory.mkdir(parents=True, exist_ok=True)
    metadata = directory / 'package-metadata.json'
    metadata.write_text(json.dumps({'version': version, 'product': product, **data}, indent=2) + '\n', encoding='utf-8')
    return metadata


# Build the Tauri bundler override from package metadata
def tauri_override(directory, env=None, platform='linux'):
    data = load(env)
    version = (ROOT / 'VERSION').read_text().strip()
    metadata = write_documents(directory, data, 'desktop', version)
    bundle = {
        'publisher': data['package']['publisher'], 'homepage': data['package']['website'],
        'license': data['package']['license'], 'copyright': data['package']['copyright'],
        'shortDescription': data['desktop']['summary'],
        'longDescription': data['desktop']['description'],
        'resources': {Path(os.path.relpath(metadata.resolve(), (ROOT / 'src-tauri').resolve())).as_posix(): 'notices/package-metadata.json'},
    }
    if platform != 'linux':
        return {'bundle': bundle}
    metainfo = directory / 'app.streamdockmanager.manager.metainfo.xml'
    metainfo.write_text(appstream(data, version), encoding='utf-8')
    files = {
        '/usr/share/metainfo/app.streamdockmanager.manager.metainfo.xml': str(metainfo.resolve()),
        '/usr/share/doc/stream-dock-manager/package-metadata.json': str(metadata.resolve()),
    }
    bundle['linux'] = {'deb': {'files': files, 'section': data['linux']['section'],
                               'priority': data['linux']['priority']},
                       'rpm': {'files': files, 'release': data['linux']['rpm_release']}}
    return {'bundle': bundle}



# Parse options and run the package metadata workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=['linux', 'macos', 'windows'], default='linux')
    parser.add_argument('--output', type=Path, help='Generate a Tauri override and its package documents')
    args = parser.parse_args()
    if args.output:
        print(json.dumps(tauri_override(args.output, platform=args.platform)))
    else:
        load()
        print('Installer package metadata is valid.')


if __name__ == '__main__':
    main()
