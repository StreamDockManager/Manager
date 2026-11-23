import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import assert from 'node:assert/strict';
const code = stripTypeScriptTypes(
    readFileSync(new URL('../src/device-preview-background.ts', import.meta.url), 'utf8'),
);
const { isWallpaperAperture, devicePreviewBackground } = await import(
    `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`
);
test('individual LCD wallpaper does not require a full-plane endpoint', () => {
    for (const kind of ['button', 'touch', 'display']) {
        assert.equal(
            isWallpaperAperture({ kind, image_slot: 0, image_width: 64, image_height: 64 }),
            true,
        );
    }
    for (const kind of ['encoder', 'led', 'button']) {
        assert.equal(isWallpaperAperture({ kind }), false);
    }
    assert.equal(
        isWallpaperAperture({
            kind: 'touch',
            image_slot: 1,
            image_width: 64,
            image_height: 64,
            preview_hidden: true,
        }),
        false,
    );
});
test('browser wallpaper follows saved candidate and cancellation without replacing custom media', () => {
    const base = {
        native: false,
        customPage: false,
        preview: false,
        paletteAsset: false,
        streamed: null,
        palette: 'saved',
    };
    assert.equal(devicePreviewBackground(base), 'saved');
    assert.equal(
        devicePreviewBackground({ ...base, preview: true, palette: 'candidate' }),
        'candidate',
    );
    assert.equal(devicePreviewBackground(base), 'saved');
    assert.equal(
        devicePreviewBackground({ ...base, customPage: true, streamed: 'custom' }),
        'custom',
    );
});
test('global palette transport has no selected-device or sleeping-device gate', async () => {
    const source = stripTypeScriptTypes(
        readFileSync(new URL('../src/plugin-palette-preview.ts', import.meta.url), 'utf8'),
    );
    const { pluginPalettePreviewContext } = await import(
        `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`
    );
    const context = {
        route: 'settings',
        followNavigation: true,
        suspended: false,
        hidden: false,
        quiescing: false,
        closing: false,
    };
    assert.deepEqual(
        pluginPalettePreviewContext(true, context),
        pluginPalettePreviewContext(true, {
            ...context,
            route: 'editor',
            selection: 'another-device',
        }),
    );
    assert.ok(pluginPalettePreviewContext(true, context));
    assert.equal(pluginPalettePreviewContext(false, context), null);
    const main = readFileSync(new URL('../src/main.ts', import.meta.url), 'utf8');
    assert.equal(main.includes("bridge.command('preview_on_dock'"), false);
    assert.match(
        main,
        /await paletteDraftRetirement;\s+return bridge.command\('preview_plugin_palette'/,
    );
});
