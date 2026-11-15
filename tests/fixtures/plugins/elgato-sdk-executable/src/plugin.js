// Project-owned conformance probe; all protocol work uses the unmodified official SDK
import streamDeck from '@elgato/streamdeck';
import { readFileSync, writeFileSync } from 'node:fs';
const mode = JSON.parse(readFileSync('mode.json', 'utf8'));
streamDeck.settings.useLegacySettingsBehavior = mode.legacy;
// Complete the deferred plugin work using the current operation state
streamDeck.actions.onWillAppear(async ({ action }) => {
    const settings = await action.getSettings();
    await action.setTitle(`ready:${settings.marker}`);
});
// Complete the deferred plugin work using the current operation state
streamDeck.actions.onKeyDown(async ({ action }) => {
    await action.setSettings({ marker: 'saved', count: 2 });
    const settings = await action.getSettings();
    await streamDeck.settings.setGlobalSettings({ fixture: 'global-roundtrip' });
    const globals = await streamDeck.settings.getGlobalSettings();
    await action.setTitle(
        JSON.stringify({ settings, globals, version: streamDeck.info.application.version }),
    );
});
try {
    await streamDeck.connect();
    writeFileSync('connection-result.json', JSON.stringify({ connected: true }));
} catch (error) {
    writeFileSync(
        'connection-result.json',
        JSON.stringify({ connected: false, error: String(error) }),
    );
    process.exitCode = 1;
}
