// SPDX-License-Identifier: GPL-3.0-or-later
// Read-only GNOME Shell 45–48 session guard. No key observation or input injection
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const NAME = 'org.streamdockmanager.SessionGuard1';
const PATH = '/org/streamdockmanager/SessionGuard1';
const XML = `<node><interface name="${NAME}">
  <method name="GetState">
    <arg name="version" type="u" direction="out"/>
    <arg name="instance" type="s" direction="out"/>
    <arg name="generation" type="t" direction="out"/>
    <arg name="protected" type="b" direction="out"/>
  </method>
  <signal name="StateChanged">
    <arg name="instance" type="s"/>
    <arg name="generation" type="t"/>
    <arg name="protected" type="b"/>
  </signal>
</interface></node>`;

export default class SessionGuard extends Extension {
    // Enable the extension operation
    enable() {
        this._connections = [];
        this._instance = GLib.uuid_string_random();
        this._generation = 1;
        this._protected = true;
        this._enabled = true;
        this._export = null;
        this._owner = 0;
        // Watch the extension operation
        const watch = (object, signal) => {
            if (!object || typeof object.connect !== 'function')
                throw new Error('Unsupported Shell session API');
            const id = object.connect(signal, () => this._refresh());
            this._connections.push([object, id]);
        };
        try {
            // Subscribe before snapshot/export. JS and these state transitions run
            // on Shell's main loop; no Eval, polling an animation flag or monkeypatch
            watch(Main.sessionMode, 'updated');
            watch(Main.screenShield, 'active-changed');
            watch(Main.screenShield, 'locked-changed');
            watch(Main.screenShield.actor, 'notify::visible');
            this._refresh();
            this._export = Gio.DBusExportedObject.wrapJSObject(XML, this);
            this._export.export(Gio.DBus.session, PATH);
            this._owner = Gio.bus_own_name_on_connection(
                Gio.DBus.session,
                NAME,
                Gio.BusNameOwnerFlags.NONE,
                null,
                () => this._fail(),
            );
        } catch (error) {
            this.disable();
            throw error;
        }
    }

    // Treat missing session signals as protected and detect lock, greeter or shield activity
    _sample() {
        const mode = Main.sessionMode;
        const shield = Main.screenShield;
        if (
            !this._enabled ||
            !mode ||
            !shield ||
            !shield.actor ||
            typeof mode.isLocked !== 'boolean' ||
            typeof mode.isGreeter !== 'boolean' ||
            typeof mode.currentMode !== 'string' ||
            mode.currentMode.length === 0 ||
            typeof shield.locked !== 'boolean' ||
            typeof shield.active !== 'boolean' ||
            typeof shield.actor.visible !== 'boolean'
        )
            return true;
        // Session mode switches during activate(), before GNOME deliberately
        // delays public ScreenSaver.ActiveChanged for its lock-screen animation
        return (
            mode.isLocked ||
            mode.isGreeter ||
            mode.currentMode === 'unlock-dialog' ||
            shield.locked ||
            shield.active ||
            shield.actor.visible
        );
    }

    // Refresh the extension operation
    _refresh() {
        let protectedState = true;
        try {
            protectedState = this._sample();
        } catch (_) {
            /* fail closed */
        }
        if (protectedState !== this._protected) {
            this._protected = protectedState;
            this._generation++;
            if (!Number.isSafeInteger(this._generation)) {
                this._fail();
                return;
            }
            this._export?.emit_signal(
                'StateChanged',
                new GLib.Variant('(stb)', [this._instance, this._generation, this._protected]),
            );
        }
    }

    // Refresh session protection and return the current instance and generation snapshot
    GetState() {
        this._refresh();
        if (!this._enabled) throw new Error('Session guard is unavailable');
        return [1, this._instance, this._generation, this._protected];
    }

    // Fail closed by announcing protection and withdrawing the session guard service
    _fail() {
        if (this._enabled && Number.isSafeInteger(this._generation + 1)) {
            this._generation++;
            this._export?.emit_signal(
                'StateChanged',
                new GLib.Variant('(stb)', [this._instance, this._generation, true]),
            );
        }
        this._enabled = false;
        this._protected = true;
        // Unexporting immediately denies new snapshots. Service/name teardown
        // invalidates pinned leases; no stale false state is returned
        this._export?.unexport();
    }

    // Disable the extension operation
    disable() {
        this._fail();
        for (const [object, id] of this._connections ?? []) object.disconnect(id);
        this._connections = [];
        if (this._owner) Gio.bus_unown_name(this._owner);
        this._owner = 0;
        this._export = null;
    }
}
