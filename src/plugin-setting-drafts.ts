import { cloneJson } from './json-clone';
import type { JsonValue } from '../generated/runtime';
// Copy the plugin setting drafts operation
const copy = (value: JsonValue): JsonValue => cloneJson(value);
export const PLUGIN_DRAFT_BYTES = 8 * 1024 * 1024;
const MAX_ENTRIES = 128;
const MAX_VALUE_BYTES = 256 * 1024;
interface Alternative {
    serialized: string;
    bytes: number;
}
/** Unsaved alternatives are protected: full storage rejects a switch, never evicts a draft. */
export class PluginSettingDrafts {
    private contexts = new Map<string, Map<string, Alternative>>();
    private bytes = 0;
    private count = 0;
    // Derive retained bytes from the current plugin setting drafts state
    get retainedBytes() {
        return this.bytes;
    }
    // Derive size from the current plugin setting drafts state
    get size() {
        return this.count;
    }
    // Retain a bounded plugin-action draft only when it differs from the fallback settings
    remember(
        context: string,
        plugin: string,
        action: string,
        value: JsonValue,
        fallback?: JsonValue,
    ) {
        const serialized = JSON.stringify(value);
        if (fallback !== undefined && serialized === JSON.stringify(fallback)) {
            this.forget(context, plugin, action);
            return;
        }
        const key = JSON.stringify([plugin, action]);
        const previous = this.contexts.get(context)?.get(key);
        if (previous?.serialized === serialized) return;
        if (
            serialized.length > MAX_VALUE_BYTES ||
            new TextEncoder().encode(serialized).byteLength > MAX_VALUE_BYTES
        )
            throw Error(
                'Plugin settings exceed the 256 KiB draft limit. Save before changing actions.',
            );
        // Charge UTF-16 storage plus keys and fixed metadata. This is a conservative
        // logical budget, not an engine-specific heap measurement
        const bytes = 2 * (serialized.length + context.length + key.length) + 128;
        if (
            this.bytes - (previous?.bytes || 0) + bytes > PLUGIN_DRAFT_BYTES ||
            (!previous && this.count >= MAX_ENTRIES)
        )
            throw Error(
                'Alternative plugin settings storage is full. Save or discard existing drafts before changing actions.',
            );
        const settings = this.contexts.get(context) || new Map<string, Alternative>();
        settings.set(key, { serialized, bytes });
        this.contexts.set(context, settings);
        this.bytes += bytes - (previous?.bytes || 0);
        if (!previous) ++this.count;
    }
    // Restore the retained plugin setting drafts state
    restore(context: string, plugin: string, action: string, fallback: JsonValue) {
        const saved = this.contexts.get(context)?.get(JSON.stringify([plugin, action]));
        return saved ? (JSON.parse(saved.serialized) as JsonValue) : copy(fallback);
    }
    // Remove the matching draft and release its item and byte budget
    forget(context: string, plugin: string, action: string) {
        const settings = this.contexts.get(context);
        const key = JSON.stringify([plugin, action]);
        const value = settings?.get(key);
        if (!value) return;
        this.bytes -= value.bytes;
        --this.count;
        settings!.delete(key);
        if (!settings!.size) this.contexts.delete(context);
    }
    // Move the selected plugin setting drafts item
    move(from: string, to: string) {
        const value = this.contexts.get(from);
        if (value && from !== to) {
            if (this.contexts.has(to))
                throw Error("Cannot replace another editor's alternative plugin settings.");
            const adjustment = 2 * (to.length - from.length);
            if (this.bytes + adjustment * value.size > PLUGIN_DRAFT_BYTES)
                throw Error(
                    'Alternative plugin settings storage is full. Save or discard existing drafts.',
                );
            this.contexts.delete(from);
            for (const entry of value.values()) entry.bytes += adjustment;
            this.bytes += adjustment * value.size;
            this.contexts.set(to, value);
        }
    }
    // Clear the retained plugin setting drafts value
    clear(context?: string) {
        if (context === undefined) {
            this.contexts.clear();
            this.bytes = 0;
            this.count = 0;
        } else {
            for (const entry of this.contexts.get(context)?.values() || []) {
                this.bytes -= entry.bytes;
                --this.count;
            }
            this.contexts.delete(context);
        }
    }
}
