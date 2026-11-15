import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

// Evaluate the actual initial snapshot expression without importing native or browser services
test('browser preview names its built-in profile Default while retaining its stable ID', () => {
    const source = stripTypeScriptTypes(
        readFileSync(new URL('../src/bridge-preview.ts', import.meta.url), 'utf8'),
    );
    const start = source.indexOf('state = {');
    assert.notEqual(start, -1);
    const rest = source.slice(start + 'state = '.length);
    const end = rest.indexOf('\n        };');
    assert.notEqual(end, -1);
    const snapshot = runInNewContext('(' + rest.slice(0, end + '\n        }'.length) + ')', {
        m: { model_id: 'fixture' },
        defaultMedia: () => ({}),
    });
    assert.equal(snapshot.active_profile, 'default');
    assert.equal(snapshot.profiles[0].id, 'default');
    assert.equal(snapshot.profiles[0].name, 'Default');
});
