import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
const html = readFileSync(new URL('../public/components/sd-plugins.html', import.meta.url), 'utf8');
// Verify health panels mount on demand, rather than retaining all hidden panel trees
test('health panels mount on demand, rather than retaining all hidden panel trees', () => {
    for (const tab of ['overview', 'events', 'compatibility', 'alerts']) {
        assert.match(
            html,
            new RegExp(
                `<template x-if="healthTab === '${tab}'">\\s*<section\\s+id="health-panel-${tab}"`,
            ),
        );
        assert.equal(html.includes(`x-show="healthTab === '${tab}'"`), false);
        assert.match(html, new RegExp(`aria-labelledby="health-tab-${tab}"`));
    }
    // Resource alert drafts live in the controller, not a panel-local x-data
    const alerts = html.slice(html.indexOf('id="health-panel-alerts"'));
    assert.match(alerts, /x-model="resourceAlerts.enabled"/);
});
