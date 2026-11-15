// Candidate-only probe. The official SDK and its default configuration are untouched
// Files are a test control/report channel; every protocol operation uses the public SDK
import streamDeck from '@elgato/streamdeck';
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';

const events = {
    appearances: [],
    disappearances: [],
    settings: [],
    globals: [],
    resources: [],
    devices: [],
    keys: [],
};
let response;
// Report the current mode operation
const report = () => {
    writeFileSync('report.tmp', JSON.stringify({ events, response }));
    renameSync('report.tmp', 'report.json');
};
// Complete the deferred current mode work using the current operation state
streamDeck.actions.onWillAppear(async ({ action, payload }) => {
    events.appearances.push({ context: action.id, settings: await action.getSettings(), payload });
    report();
});
streamDeck.actions.onWillDisappear(({ action, payload }) => {
    events.disappearances.push({ context: action.id, payload });
    report();
});
// Complete the deferred current mode work using the current operation state
streamDeck.settings.onDidReceiveSettings(async ({ action, payload }) => {
    events.settings.push({
        context: action.id,
        payload: payload.settings,
        eventPayload: payload,
        cached: await action.getSettings(),
    });
    report();
});
streamDeck.actions.onKeyDown(({ action, payload }) => {
    events.keys.push({ context: action.id, payload });
    report();
});
streamDeck.settings.onDidReceiveGlobalSettings(({ settings }) => {
    events.globals.push(settings);
    report();
});
streamDeck.actions.onDidReceiveResources(({ action, payload }) => {
    events.resources.push({ context: action.id, resources: payload.resources });
    report();
});
streamDeck.devices.onDeviceDidChange(({ device }) => {
    events.devices.push({ id: device.id, name: device.name, size: device.size });
    report();
});

try {
    await streamDeck.connect();
    response = {
        sequence: 0,
        connected: true,
        legacy: streamDeck.settings.useLegacySettingsBehavior,
        version: streamDeck.info.application.version,
    };
    report();
} catch (error) {
    response = { sequence: 0, connected: false, error: String(error) };
    report();
    throw error;
}

let sequence = 0;
let busy = false;
// Refresh current mode state on the scheduled interval
setInterval(async () => {
    if (busy || !existsSync('command.json')) return;
    const command = JSON.parse(readFileSync('command.json', 'utf8'));
    if (command.sequence <= sequence) return;
    sequence = command.sequence;
    busy = true;
    try {
        const action = command.context
            ? streamDeck.actions.getActionById(command.context)
            : undefined;
        let result;
        switch (command.operation) {
            case 'query': {
                const settings = await action.getSettings();
                settings.clientMutation = true;
                result = {
                    settings: await action.getSettings(),
                    globals: await streamDeck.settings.getGlobalSettings(),
                };
                break;
            }
            case 'selfWrite':
                await action.setSettings(command.settings);
                result = { settings: await action.getSettings() };
                await streamDeck.settings.setGlobalSettings({ source: 'runtime' });
                result.globals = await streamDeck.settings.getGlobalSettings();
                break;
            case 'setResourcesOnly':
                await action.setResources(command.resources);
                result = { sent: true };
                break;
            case 'resourceRoundtrip':
                await action.setResources(command.resources);
                result = { resources: await action.getResources() };
                break;
            case 'queryResources':
                result = { resources: await action.getResources() };
                break;
            default:
                throw new Error(`Unknown fixture command: ${command.operation}`);
        }
        response = { sequence, result };
    } catch (error) {
        response = { sequence, error: String(error) };
    } finally {
        busy = false;
        report();
    }
}, 10);
