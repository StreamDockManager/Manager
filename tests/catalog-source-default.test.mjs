import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
// Exercise source defaults and saved preferences without starting browser or network services
const code = stripTypeScriptTypes(
    readFileSync(new URL('../src/update-preference-drafts.ts', import.meta.url), 'utf8'),
)
    .replace(/^import .*;$/gm, '')
    .replace(/^export /gm, '');
const { CatalogUpdateDraft } = runInNewContext(code + '\n({ CatalogUpdateDraft })');
test('catalog defaults to GitHub and preserves explicit CDN and older custom URLs', () => {
    for (const [saved, expected] of [
        [{}, 'repository'],
        [{ plugins_catalog_source: 'hosted' }, 'hosted'],
        [{ plugins_catalog_url: 'https://cdn.example/catalog.json' }, 'hosted'],
        [
            {
                plugins_catalog_source: 'repository',
                plugins_catalog_url: 'https://cdn.example/catalog.json',
            },
            'repository',
        ],
    ]) {
        const preferences = {
            plugins_repository: 'StreamDockManager/Plugins',
            plugin_channel: 'stable',
            ...saved,
        };
        const draft = new CatalogUpdateDraft();
        draft.load(preferences);
        assert.equal(draft.source, expected);
        assert.equal(draft.dirty(preferences), false);
        draft.source = expected === 'hosted' ? 'repository' : 'hosted';
        assert.equal(draft.dirty(preferences), true);
    }
});
