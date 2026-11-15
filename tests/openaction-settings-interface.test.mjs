import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import vm from 'node:vm';
import { pluginAuthorLabel } from '../src/plugin-author.ts';
import { pluginSdkLabel } from '../src/plugin-sdk-label.ts';

const source = stripTypeScriptTypes(
    readFileSync(new URL('../src/route-plugins.ts', import.meta.url), 'utf8'),
)
    .replace(/^import[\s\S]*?;\n/gm, '')
    .replace('export function register()', 'function register()');
// Instantiate plugin management against recorded commands and isolated recovery state
function setup() {
    let factory;
    const calls = [];
    const recovery = { status: { active: false } };
    const plugin = {
        id: 'openaction.fixture',
        dialect: 'openaction',
        enabled: true,
        has_settings_interface: true,
        runtime: { state: 'running' },
    };
    const state = {
        snapshot: { plugins: [plugin] },
        // Dispatch the requested openaction settings interface tests command
        async command(...args) {
            calls.push(args);
        },
    };
    vm.runInNewContext(`${source}\nregister();`, {
        Alpine: {
            // Capture the Alpine component factory for direct test instantiation
            data: (_name, value) => {
                factory = value;
            },
            // Expose the test recovery store to the component
            store: () => recovery,
        },
        // Expose the fixture application state to plugin management
        app: () => state,
        // Stub unrelated plugin-organization behavior for the focused settings-interface test
        pluginChoiceOrganization: () => ({}),
        // Register the installed package export
        registerInstalledPackageExport() {},
        // Stub resource-alert editing outside the settings-interface test scope
        resourceAlertDraft: () => ({}),
        // Ignore runtime label formatting in the focused settings-interface test
        pluginRuntimeLabel() {},
        pluginAuthorLabel,
        pluginSdkLabel,
        performance,
    });
    return { controller: factory(), plugin, calls, recovery, state };
}
// Handle the openaction settings interface tests click interaction
const click = (id = 'openaction.fixture') => ({ currentTarget: { dataset: { id } } });

// Verify OpenAction settings button sends only the native settings-interface command
test('OpenAction settings button sends only the native settings-interface command', async () => {
    const { controller, plugin, calls } = setup();
    assert.equal(controller.dialectLabel(plugin), 'OpenActionsAPI');
    assert.equal(controller.canShowSettingsInterface(plugin), true);
    await controller.showSettingsInterface(click());
    assert.equal(calls.length, 1);
    assert.equal(calls[0][0], 'plugin_show_settings_interface');
    assert.equal(calls[0][1].id, plugin.id);
});

// Verify settings interfaces cannot launch disabled, stopped, unadvertised or recovery plugins
test('settings interfaces cannot launch disabled, stopped, unadvertised or recovery plugins', async () => {
    for (const changes of [
        { enabled: false },
        { has_settings_interface: false },
        { has_settings_interface: undefined },
        // Verify settings interfaces cannot launch disabled, stopped, unadvertised or recovery plugins
        ...['idle', 'starting', 'blocked', 'stopped'].map((state) => ({ runtime: { state } })),
    ]) {
        const { controller, plugin, calls } = setup();
        Object.assign(plugin, changes);
        assert.equal(controller.canShowSettingsInterface(plugin), false);
        await controller.showSettingsInterface(click());
        assert.equal(calls.length, 0);
    }
    const { controller, plugin, calls, recovery } = setup();
    recovery.status.active = true;
    assert.equal(controller.canShowSettingsInterface(plugin), false);
    await controller.showSettingsInterface(click());
    await controller.showSettingsInterface(click('missing'));
    assert.equal(calls.length, 0);
});

// Verify live runtime state overrides stale plugin snapshot and command failures remain handled
test('live runtime state overrides stale plugin snapshot and command failures remain handled', async () => {
    const { controller, plugin, calls, state } = setup();
    controller.live[plugin.id] = { enabled: true, runtime: { state: 'idle' } };
    assert.equal(controller.canShowSettingsInterface(plugin), false);
    await controller.showSettingsInterface(click());
    assert.equal(calls.length, 0);
    controller.live[plugin.id].runtime.state = 'running';
    // Verify live runtime state overrides stale plugin snapshot and command failures remain handled
    state.command = async () => {
        throw new Error('Plugin disconnected');
    };
    await assert.doesNotReject(controller.showSettingsInterface(click()));
});

// Verify template retains existing plugin settings and gates the separate native interface
test('template retains existing plugin settings and gates the separate native interface', () => {
    const template = readFileSync(
        new URL('../public/components/sd-plugins.html', import.meta.url),
        'utf8',
    );
    assert.match(template, /@click="settings"/);
    assert.match(
        template,
        /x-show="plugin\.has_settings_interface"[\s\S]*?:disabled="!canShowSettingsInterface\(plugin\)"[\s\S]*?@click="showSettingsInterface"/,
    );
});

// Verify installed-source import omits Stream Deck but manual compatibility remains
test('installed-source import omits Stream Deck but manual compatibility remains', () => {
    // Verify installed-source import omits Stream Deck but manual compatibility remains
    const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');
    const folder = read('../public/components/sd-plugin-folder-import.html');
    assert.match(folder, /value="vsd"/);
    assert.match(folder, /@click="choosePluginFolder"/);
    assert.doesNotMatch(folder, /value="streamdeck"|protected_streamdeck|Elgato/);
    assert.match(read('../src/updates.ts'), /'streamDeckPlugin'/);
    assert.match(read('../public/components/sd-settings.html'), /pluginLinksEnabled/);
    const { controller, plugin } = setup();
    assert.equal(controller.dialectLabel({ ...plugin, dialect: 'streamdeck' }), 'Stream Deck');
});
