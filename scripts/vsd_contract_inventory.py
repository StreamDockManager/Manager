#!/usr/bin/env python3
"""Derive VSD review rows from pinned identifiers, never execute vendor sources.

The documentation snapshot contains factual API/member names, not vendor prose.
Raw website response hashes identify the reviewed content; mutable documentation
has no stable release revision and is not a claim of exhaustive conformance.
"""
import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1] / 'tests/fixtures/plugins'
DOCS_PATH = 'vendor/vsd/documentation-facts.json'
DEFINES_PATH = 'vendor/vsd/VSDCPPSDK/StreamDockCPPSDK/StreamDockSDK/HSDSDKDefines.h'


class DocumentationIdentifiers(HTMLParser):
    # Initialize section, table-row, and identifier tracking for documentation parsing
    def __init__(self):
        super().__init__()
        self.section = ''
        self.capture = None
        self.text = []
        self.row = []
        self.headings = []
        self.members = []

    # Collect structured documentation fields from an opening HTML tag
    def handle_starttag(self, tag, attrs):
        if tag in ('h2', 'td'):
            self.capture = tag
            self.text = []
        elif tag == 'tr':
            self.row = []

    # Retain relevant documentation text within the current parsed element
    def handle_data(self, data):
        if self.capture:
            self.text.append(data)

    # Finalize the structured field when its HTML element closes
    def handle_endtag(self, tag):
        if tag == self.capture:
            value = ''.join(self.text).strip()
            if tag == 'h2':
                self.section = value
                if value not in self.headings:
                    self.headings.append(value)
            else:
                self.row.append(value)
            self.capture = None
        if tag == 'tr' and self.row and self.section:
            name = self.row[0]
            if re.fullmatch(r'[A-Za-z][A-Za-z0-9]*', name):
                requiredness = self.row[1] if len(self.row) > 1 and self.row[1] in ('Required', 'Optional') else None
                self.members.append({'section': self.section, 'name': name, 'requiredness': requiredness})


# Fetch and validate the upstream documentation snapshot for contract extraction
def documentation_snapshot(html_directory, reviewed_at):
    pages = []
    for page in ('manifest', 'events-sent', 'events-received'):
        raw = (html_directory / (page + '.html')).read_bytes()
        parser = DocumentationIdentifiers()
        parser.feed(raw.decode('utf-8'))
        if not parser.headings or not parser.members:
            raise ValueError('Missing documented identifiers: ' + page)
        item = {'page': page, 'source_url': f'https://sdk.key123.vip/en/guide/{page}.html',
                'source_sha256': hashlib.sha256(raw).hexdigest(), 'reviewed_at': reviewed_at}
        if page == 'manifest':
            item['members'] = [member for member in parser.members if member['requiredness']]
        else:
            item['events'] = [name for name in parser.headings if re.fullmatch(r'[a-z][A-Za-z0-9]*', name)]
        pages.append(item)
    return {'schema_version': 1,
            'provenance': 'Official VSD repository README links creator.key123.vip, whose Getting Started page links sdk.key123.vip. Only factual identifiers/requiredness are retained. Website prose and executable scripts are not redistributed.',
            'limitations': 'Mutable documentation snapshot, not a versioned complete vendor schema. Source response hashes identify reviewed pages but their original HTML is not packaged. Native/reference-host semantics and undocumented APIs remain unqualified.',
            'pages': pages}


# Extract pinned protocol identifiers from documentation and SDK definitions
def inventory(root=ROOT):
    docs = json.loads((root / DOCS_PATH).read_text())
    doc_lines = (root / DOCS_PATH).read_text().splitlines()
    rows = {}

    # Extract the documented identifier from the current table row
    def row(kind, name, source, line, documentation_url=None, requiredness=None):
        key = (kind, name)
        if key in rows:
            rows[key].setdefault('additional_sources', []).append({'source': source, 'source_line': line})
            return
        rows[key] = {'id': f'vsd.{kind}.{name}', 'vendor': 'vsd', 'kind': kind, 'name': name,
                     'direction': {'command': 'plugin-to-host', 'event': 'host-to-plugin', 'manifest-field': 'package-to-host'}[kind],
                     'required': True, 'status': 'unverified', 'source': source, 'source_line': line,
                     'payload_contract': 'Identifier/member declared in pinned official documentation facts or MIT SDK source; complete payload constraints require separate qualification.',
                     'version_gate': 'VSD SDK 1 source baseline; per-feature host version gates have not been qualified.',
                     'implementation': '', 'tests': '',
                     'blocker': 'Vendor-specific payload, admission and reference-host behavior are not independently qualified.'}
        if documentation_url:
            rows[key]['documentation_url'] = documentation_url
        if requiredness:
            rows[key]['documented_requiredness'] = requiredness

    cursor = 0
    for page in docs['pages']:
        url = page['source_url']
        if page['page'] == 'manifest':
            prefixes = {'Members': '', 'Actions': 'Actions[].', 'States': 'Actions[].States[].',
                        'OS': 'OS[].', 'Software': 'Software.', 'Nodejs': 'Nodejs.'}
            for member in page['members']:
                prefix = prefixes[member['section']]
                name = member['name']
                needle = '"name": ' + json.dumps(name)
                line = next(i for i in range(cursor, len(doc_lines)) if needle in doc_lines[i])
                cursor = line + 1
                row('manifest-field', prefix + name, DOCS_PATH, line + 1, url, member['requiredness'])
        else:
            for name in page['events']:
                line = next(i for i in range(cursor, len(doc_lines)) if json.dumps(name) in doc_lines[i])
                cursor = line + 1
                row('command' if page['page'] == 'events-sent' else 'event', name, DOCS_PATH, line + 1, url)
    text = (root / DEFINES_PATH).read_text()
    functions = text.index('// Functions')
    payloads = text.index('// Payloads')
    for match in re.finditer(r'#define\s+kESDSDKEvent\w+\s+(?:\\\s*)?"([A-Za-z]+)"', text):
        if match.start() >= payloads:
            continue
        row('command' if match.start() > functions else 'event', match.group(1),
            DEFINES_PATH, text.count('\n', 0, match.start()) + 1)
    if not rows or not any(kind == 'manifest-field' for kind, _ in rows):
        raise ValueError('VSD source inventory is empty or incomplete')
    return list(rows.values())


# Parse options and run the vsd contract inventory workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--snapshot-html', type=Path, help='Explicitly extract reviewed local HTML files; no network fetch')
    parser.add_argument('--reviewed-at', help='Required YYYY-MM-DD review date with --snapshot-html')
    args = parser.parse_args()
    if args.snapshot_html:
        if not args.reviewed_at or not re.fullmatch(r'\d{4}-\d{2}-\d{2}', args.reviewed_at):
            parser.error('--snapshot-html requires --reviewed-at YYYY-MM-DD')
        (ROOT / DOCS_PATH).write_text(json.dumps(documentation_snapshot(args.snapshot_html, args.reviewed_at), indent=4) + '\n')
    generated = inventory()
    path = ROOT / 'contract-inventory.json'
    contracts = json.loads(path.read_text())
    existing = [row for row in contracts['rows'] if row['id'].startswith(('vsd.command.', 'vsd.event.', 'vsd.manifest-field.'))]
    reviewed = {row['id']: row for row in existing}
    for row in generated:
        if row['id'] in reviewed:
            for field in ('status', 'implementation', 'tests', 'blocker', 'version_gate'):
                row[field] = reviewed[row['id']][field]
    if args.check:
        if existing != generated:
            raise SystemExit('VSD inventory is stale; rerun scripts/vsd_contract_inventory.py after source review')
    else:
        contracts['rows'] = [row for row in contracts['rows'] if row not in existing] + generated
        path.write_text(json.dumps(contracts, indent=4) + '\n')
    print(f'{len(generated)} VSD command/event/manifest review rows; no completeness or runtime qualification claim')


if __name__ == '__main__':
    main()
