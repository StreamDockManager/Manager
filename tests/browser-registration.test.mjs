import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(
    new URL('../crates/plugin-host/src/browser_environment.js', import.meta.url),
    'utf8',
)
    .trim()
    .replace(/;$/, '');
const resolve = vm.runInNewContext(`(${source})`);
// Provide deterministic Linux host registration metadata
const info = () => ({
    application: { language: 'en', platform: 'linux', version: '6.9.0' },
    devicePixelRatio: 2,
});

// Verify browser PI uses its own real scale without changing native context or host language
test('browser PI uses its own real scale without changing native context or host language', () => {
    const native = info();
    const result = resolve(native, {
        devicePixelRatio: 1.25,
        language: 'ja-JP',
        languages: ['ja-JP'],
    });
    assert.equal(result.devicePixelRatio, 1.25);
    assert.equal(result.application.language, 'en');
    assert.equal(result.application.platform, 'linux');
    assert.equal(native.devicePixelRatio, 2);
    assert.notEqual(result.application, native.application);
});

// Verify unknown or invalid browser display scale prevents registration instead of inventing 1x
test('unknown or invalid browser display scale prevents registration instead of inventing 1x', () => {
    for (const scale of [undefined, null, '2', NaN, Infinity, 0, -1, 17]) {
        assert.throws(
            // Verify unknown or invalid browser display scale prevents registration instead of inventing 1x
            () => resolve(info(), { devicePixelRatio: scale }),
            /actual browser display scale/,
        );
    }
    assert.equal(resolve(info(), { devicePixelRatio: 1 }).devicePixelRatio, 1);
    assert.equal(resolve(info(), { devicePixelRatio: 2.5 }).devicePixelRatio, 2.5);
});

// Verify browser locale cannot supply or spoof an unknown host UI language
test('browser locale cannot supply or spoof an unknown host UI language', () => {
    for (const language of [undefined, '', 'unsupported', '<script>']) {
        const value = info();
        value.application.language = language;
        assert.throws(
            // Verify browser locale cannot supply or spoof an unknown host UI language
            () => resolve(value, { devicePixelRatio: 2, languages: ['en'] }),
            /host application language/,
        );
    }
});
