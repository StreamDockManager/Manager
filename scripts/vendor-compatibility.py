#!/usr/bin/env python3
"""Validate pinned vendor evidence. Full qualification is a separate failing-closed gate."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / 'tests/fixtures/plugins'
STATUSES = {'verified', 'missing', 'partial', 'unverified', 'platform/device-limited'}


# Validate the selected metadata against the module’s supported contract
def validate(root=ROOT, require_full=False):
    matrix = json.loads((root / 'compatibility-matrix.json').read_text())
    if matrix.get('schema_version') != 1 or not isinstance(matrix.get('full_compatibility_claim'), bool):
        raise ValueError('Unsupported compatibility matrix')
    sources = root / matrix['baseline_sources']
    if sources.resolve().parent != root.resolve():
        raise ValueError('Source inventory must be within the fixture directory')
    inventory = json.loads(sources.read_text())
    if inventory.get('schema_version') != 1 or not inventory.get('sources') or not inventory.get('files'):
        raise ValueError('Missing pinned source inventory')
    seen_files = set()
    for item in inventory['files']:
        path = root / item['path']
        if not path.resolve().is_relative_to(root.resolve()) or item['path'] in seen_files:
            raise ValueError('Invalid or duplicate source path')
        seen_files.add(item['path'])
        if hashlib.sha256(path.read_bytes()).hexdigest() != item['sha256']:
            raise ValueError('Source hash mismatch: ' + item['path'])
    ids, gaps = set(), []
    if not matrix.get('rows'):
        raise ValueError('Empty compatibility inventory')
    for row in matrix['rows']:
        if not row['id'] or row['id'] in ids or row['status'] not in STATUSES or not isinstance(row['required'], bool):
            raise ValueError('Invalid or duplicate compatibility row')
        ids.add(row['id'])
        if not row['area'] or (row['status'] in {'verified', 'partial'} and not row['evidence']):
            raise ValueError('Missing evidence: ' + row['id'])
        if row['status'] != 'verified' and not row['blocker']:
            raise ValueError('Missing limitation: ' + row['id'])
        if row['required'] and row['status'] != 'verified':
            gaps.append(row['id'] + ': ' + row['status'] + ' (' + row['blocker'] + ')')
    contracts = json.loads((root / 'contract-inventory.json').read_text())
    if contracts.get('schema_version') != 1 or not isinstance(contracts.get('inventory_complete'), bool) or not contracts.get('rows'):
        raise ValueError('Unsupported detailed contract inventory')
    if contracts.get('baseline') != matrix['baseline_sources']:
        raise ValueError('Detailed inventory uses a different source baseline')
    if not contracts['inventory_complete']:
        if not contracts.get('limitations'):
            raise ValueError('Incomplete contract inventory needs explicit limitations')
        gaps.append('Detailed contract inventory is incomplete')
    contract_ids = set()
    for row in contracts['rows']:
        if not row['id'] or row['id'] in contract_ids or row['status'] not in STATUSES or not isinstance(row['required'], bool):
            raise ValueError('Invalid or duplicate detailed contract row')
        contract_ids.add(row['id'])
        additional = row.get('additional_sources', [])
        if not isinstance(additional, list):
            raise ValueError('Invalid additional contract sources: ' + row['id'])
        for source in [row, *additional]:
            if not isinstance(source, dict) or source.get('source') not in seen_files or type(source.get('source_line')) is not int or source['source_line'] < 1:
                raise ValueError('Unpinned or invalid contract source: ' + row['id'])
            if source['source_line'] > len((root / source['source']).read_text().splitlines()):
                raise ValueError('Contract source line is out of range: ' + row['id'])
        if not all(row.get(key) for key in ['vendor', 'kind', 'name', 'direction', 'payload_contract', 'version_gate']):
            raise ValueError('Missing detailed contract metadata: ' + row['id'])
        if row['status'] == 'verified' and not all(row.get(key) for key in ['implementation', 'tests']):
            raise ValueError('Verified contract lacks implementation/tests: ' + row['id'])
        if row['status'] != 'verified' and not row.get('blocker'):
            raise ValueError('Missing detailed contract limitation: ' + row['id'])
        if row['required'] and row['status'] != 'verified':
            gaps.append(row['id'] + ': ' + row['status'])
    if gaps and (require_full or matrix['full_compatibility_claim']):
        raise ValueError('Full vendor compatibility is not qualified:\n' + '\n'.join(gaps))
    return {'rows': len(ids), 'files': len(seen_files), 'contracts': len(contract_ids), 'full_compatibility': not gaps, 'remaining': gaps}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--require-full', action='store_true')
    args = parser.parse_args()
    try:
        print(json.dumps(validate(require_full=args.require_full), indent=2))
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, str(error) + '\n')
