// Independently authored candidate-only driver, not the upstream Lights-Out program
// The official SDK, its defaults and the distributed profile archive are unchanged
import streamDeck from '@elgato/streamdeck';
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';

const appearances = [];
const calls = [];
let response;
// Report the bundled profiles operation
const report = () => {
    writeFileSync('report.tmp', JSON.stringify({ appearances, calls, response }));
    renameSync('report.tmp', 'report.json');
};
streamDeck.actions.onWillAppear(({ action }) => {
    appearances.push(action.id);
    report();
});

await streamDeck.connect();
response = {
    sequence: 0,
    connected: true,
    legacy: streamDeck.settings.useLegacySettingsBehavior,
    version: streamDeck.info.application.version,
    pid: process.pid,
};
report();

let sequence = 0;
let busy = false;
// Refresh bundled profiles state on the scheduled interval
setInterval(async () => {
    if (busy || !existsSync('command.json')) return;
    const command = JSON.parse(readFileSync('command.json', 'utf8'));
    if (command.sequence <= sequence) return;
    sequence = command.sequence;
    busy = true;
    try {
        // Missing JSON fields remain undefined, exercising the real optional API
        const { device, profile, page } = command;
        await streamDeck.profiles.switchToProfile(device, profile, page);
        calls.push({ device, profile, page });
        // SDK completion means sent, not that a review or transaction succeeded
        response = { sequence, sent: true };
    } catch (error) {
        response = { sequence, error: String(error) };
    } finally {
        busy = false;
        report();
    }
}, 10);
