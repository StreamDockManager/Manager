import { keyboardMacroSummary, validateKeyboardMacro } from './keyboard-macro-draft';
import type { Action, Snapshot, Input } from './types';
import type { Condition } from '../generated/runtime';
/** Deterministic, memory-only counterpart of native execution. Never accesses OS resources */
export class BuiltinPreview {
    private generation = 0;
    private slots = new Map<string, string>();
    private files = new Map<string, string>();
    private indices = new Map<string, number>();
    private cooldowns = new Map<string, number>();
    private presses = new Map<string, { pressed: boolean }>();
    private history = new Map<string, string[]>();
    private paused = new Map<string, string>();
    private clipboard = 'Preview selected text';
    private taps = new Map<string, { count: number }>();
    // Record simulated control input for running preview actions
    observe(device: string, control: string, input: Input) {
        const key = `${device}:${control}`;
        if (input.kind === 'button') {
            if (input.pressed) this.presses.set(key, { pressed: true });
            else {
                const press = this.presses.get(key);
                if (press) press.pressed = false;
            }
        }
    }
    // Resume simulated execution for a device
    resume(device: string) {
        this.paused.delete(device);
    }
    // Execute built-in actions against the browser-preview workspace
    async run(
        action: Action,
        snapshot: Snapshot,
        device: string,
        control: string,
        select: (profile: string, page?: string, manual?: boolean) => void,
        input: Input = { kind: 'pulse' },
    ): Promise<string> {
        if (this.paused.has(device) && this.paused.get(device) !== control)
            return 'Preview bindings are paused';
        if (this.paused.get(device) === control) {
            this.paused.delete(device);
            return 'Preview bindings resumed';
        }
        const generation = this.generation;
        const press = this.presses.get(`${device}:${control}`);
        const locals = new Map<string, unknown>();
        const macros = structuredClone(snapshot.saved_macros || []);
        const calls = new Set<string>();
        const effects: string[] = [];
        // Stop preview execution when its generation has been invalidated
        const check = () => {
            if (this.generation !== generation) throw Error('Preview action cancelled');
        };
        // Wait between simulated steps while honoring cancellation
        const sleep = async (ms: number) => {
            await new Promise((r) => setTimeout(r, Math.max(0, Math.min(ms, 100))));
            check();
        };
        // Wait for simulated control release with a bounded timeout
        const release = async (timeout = 30000) => {
            if (!press) throw Error('Requires physical button release events');
            const start = Date.now();
            while (press.pressed) {
                if (Date.now() - start > timeout) throw Error('Preview release timed out');
                await sleep(10);
            }
        };
        // Resolve the currently active simulated session
        const session = () => {
            const d = snapshot.devices.find((d) => d.id === device);
            return snapshot.sessions[d?.persistence_key || device];
        };
        // Resolve the profile active on the simulated device
        const currentProfile = () =>
            snapshot.profiles.find(
                (p) => p.id === (session()?.profile_id || snapshot.active_profile),
            );
        // Expand run-output placeholders when text templating is enabled
        const text = (value: string, template = false, outputs = locals) =>
            template
                ? // Replace a run-output placeholder with its simulated value
                  value.replace(/\$\{run\.([^}]+)\}/g, (_match, name) => {
                      const value = outputs.get(name);
                      if (value === undefined) throw Error(`Unknown variable ${name}`);
                      return typeof value === 'string' ? value : JSON.stringify(value);
                  })
                : value;
        // Evaluate a condition against simulated workspace state
        const condition = (c: Condition) => {
            const value =
                c.source === 'clipboard'
                    ? this.clipboard
                    : c.source === 'file'
                      ? this.files.has(c.subject)
                      : c.source === 'process'
                        ? false
                        : 'preview.application';
            switch (c.operator) {
                case 'equals':
                    return value === c.value;
                case 'not_equals':
                    return value !== c.value;
                case 'contains':
                    return String(value).includes(String(c.value));
                case 'exists':
                    return typeof value === 'boolean' ? value : value !== undefined;
                case 'missing':
                    return typeof value === 'boolean' ? !value : value === undefined;
                case 'greater':
                    return Number(value) > Number(c.value);
                case 'less':
                    return Number(value) < Number(c.value);
                default:
                    throw Error('Unknown comparison');
            }
        };
        // Interpret one built-in action without native side effects
        const execute = async (
            action: Action,
            depth = 0,
            path = 'root',
            outputs = locals,
        ): Promise<void> => {
            // Interpolate text using the current action’s output scope
            const interpolate = (value: string, template = false) => text(value, template, outputs);
            check();
            if (depth > 8) throw Error('Preview nesting exceeds 8');
            const key = `${device}:${control}:${path}`;
            if (action.kind === 'dial') {
                const lane =
                    input.kind === 'rotate'
                        ? input.delta < 0
                            ? 0
                            : input.delta > 0
                              ? 1
                              : -1
                        : input.kind === 'pulse' || (input.kind === 'button' && input.pressed)
                          ? 2
                          : -1;
                if (lane >= 0 && action.actions[lane])
                    await execute(action.actions[lane], depth + 1, `${path}.${lane}`, outputs);
                return;
            }
            if (action.kind === 'page') {
                const profile = currentProfile();
                if (profile?.pages.some((page) => page.id === action.page_id))
                    select(profile.id, action.page_id);
                effects.push('page');
                return;
            }
            if (action.kind === 'profile') {
                const target = snapshot.profiles.find(
                    (profile) => profile.id === action.profile_id,
                );
                if (target) select(target.id, target.active_page);
                effects.push('profile');
                return;
            }
            if (action.kind === 'navigate') {
                const profile = currentProfile();
                const delta = action.direction === 'previous' ? -1 : 1;
                if (profile && action.target === 'page' && profile.pages.length) {
                    const index = profile.pages.findIndex(
                        (p) => p.id === (session()?.page_id || profile.active_page),
                    );
                    const page =
                        profile.pages[
                            (Math.max(0, index) + delta + profile.pages.length) %
                                profile.pages.length
                        ]!;
                    select(profile.id, page.id);
                } else if (profile) {
                    const profiles = snapshot.profiles.filter(
                        (p) => p.model_id === profile.model_id,
                    );
                    const index = profiles.findIndex((p) => p.id === profile.id);
                    const target =
                        profiles[(Math.max(0, index) + delta + profiles.length) % profiles.length];
                    if (target) select(target.id, target.active_page);
                }
                effects.push(`${action.direction} ${action.target}`);
                return;
            }
            if (action.kind === 'macro') {
                for (let i = 0; i < action.actions.length; i++)
                    await execute(action.actions[i], depth + 1, `${path}.${i}`, outputs);
                return;
            }
            if (action.kind === 'toggle') {
                const index = this.indices.get(key) || 0;
                await execute(
                    index % 2 === 0 ? action.on : action.off,
                    depth + 1,
                    `${path}.${index % 2}`,
                    outputs,
                );
                this.indices.set(key, index + 1);
                return;
            }
            if (action.kind === 'delay') {
                await sleep(action.milliseconds);
                return;
            }
            if (action.kind !== 'builtin') {
                effects.push(action.kind);
                return;
            }
            const a = action.action;
            // Run one branch using its own output context
            const branch = async (children: Action[], i: number) => {
                if (!children[i]) throw Error('Missing action branch');
                await execute(children[i], depth + 1, `${path}.${i}`, outputs);
            };
            // Run child actions in sequence within the preview
            const sequence = async (children: Action[]) => {
                for (let i = 0; i < children.length; i++) await branch(children, i);
            };
            switch (a.kind) {
                case 'set_clipboard':
                    this.clipboard = interpolate(a.text, a.template);
                    break;
                case 'clear_clipboard':
                    this.clipboard = '';
                    break;
                case 'clipboard_slot':
                    if (a.operation === 'save') this.slots.set(a.name, this.clipboard);
                    else if (a.operation === 'restore') {
                        if (!this.slots.has(a.name)) throw Error('Preview clipboard slot is empty');
                        this.clipboard = this.slots.get(a.name)!;
                    } else this.slots.delete(a.name);
                    break;
                case 'type_text':
                    effects.push(`Type: ${interpolate(a.text, a.template)}`);
                    break;
                case 'paste_text':
                    effects.push(`Paste: ${interpolate(a.text, a.template)}`);
                    if (!a.restore) this.clipboard = interpolate(a.text, a.template);
                    break;
                case 'input_prompt':
                    outputs.set(a.output, 'Preview input');
                    break;
                case 'choose_path':
                    outputs.set(
                        a.output,
                        a.directory ? '/preview/folder' : '/preview/selected.txt',
                    );
                    break;
                case 'command_result':
                    outputs.set(a.output, { stdout: 'Preview output', stderr: '', exit_status: 0 });
                    outputs.set(`${a.output}.stdout`, 'Preview output');
                    outputs.set(`${a.output}.stderr`, '');
                    outputs.set(`${a.output}.exit_status`, 0);
                    break;
                case 'write_file': {
                    const path = interpolate(a.path, a.template);
                    if (a.operation === 'create' && this.files.has(path))
                        throw Error('Preview destination exists');
                    const value = interpolate(a.text, a.template);
                    this.files.set(
                        path,
                        a.operation === 'append' ? (this.files.get(path) || '') + value : value,
                    );
                    break;
                }
                case 'file_operation': {
                    const source = interpolate(a.source, a.template),
                        destination = interpolate(a.destination, a.template);
                    if (!this.files.has(source)) this.files.set(source, 'Preview file contents');
                    if (a.operation === 'trash') this.files.delete(source);
                    else {
                        if (this.files.has(destination) && !a.replace)
                            throw Error('Preview destination exists');
                        this.files.set(destination, this.files.get(source)!);
                        if (a.operation !== 'copy') this.files.delete(source);
                    }
                    break;
                }
                case 'cycle_actions': {
                    const index = this.indices.get(key) || 0;
                    await branch(a.children, index % a.children.length);
                    this.indices.set(key, index + 1);
                    break;
                }
                case 'repeat_actions':
                    for (let i = 0; i < a.count; i++) {
                        await sequence(a.children);
                        if (a.interval_ms) await sleep(a.interval_ms);
                    }
                    break;
                case 'repeat_held':
                    if (!press) throw Error('Requires button release events');
                    while (press.pressed) {
                        await sequence(a.children);
                        await sleep(a.interval_ms);
                    }
                    break;
                case 'conditional':
                    await branch(a.children, condition(a.condition) ? 0 : 1);
                    break;
                case 'wait_condition': {
                    const start = Date.now();
                    while (!condition(a.condition)) {
                        if (Date.now() - start >= a.timeout_ms)
                            throw Error('Preview condition timed out');
                        await sleep(a.poll_ms);
                    }
                    break;
                }
                case 'wait_release':
                    await release(a.timeout_ms);
                    break;
                case 'hold_keys':
                    await release();
                    break;
                case 'press_release':
                    await branch(a.children, 0);
                    await release();
                    await branch(a.children, 1);
                    break;
                case 'gesture': {
                    const start = Date.now();
                    while (press?.pressed && Date.now() - start < a.hold_ms) await sleep(10);
                    if (press?.pressed) {
                        await branch(a.children, 2);
                        await release();
                    } else if (this.taps.has(key)) {
                        this.taps.get(key)!.count++;
                    } else {
                        const taps = { count: 1 };
                        this.taps.set(key, taps);
                        await new Promise((r) => setTimeout(r, a.double_ms));
                        check();
                        this.taps.delete(key);
                        await branch(a.children, taps.count > 1 ? 1 : 0);
                    }
                    break;
                }
                case 'failure_policy': {
                    const attempts = a.operation === 'retry' ? a.retries + 1 : 1;
                    for (let attempt = 0; attempt < attempts; attempt++) {
                        try {
                            await branch(a.children, 0);
                            break;
                        } catch (error) {
                            check();
                            if (String(error).includes('Cancelled')) throw error;
                            if (a.operation === 'continue') break;
                            if (a.operation === 'fallback') {
                                await branch(a.children, 1);
                                break;
                            }
                            if (attempt + 1 === attempts) throw error;
                            await sleep(a.delay_ms);
                        }
                    }
                    break;
                }
                case 'parallel': {
                    const tasks = a.children.map((child, i) =>
                        execute(child, depth + 1, `${path}.${i}`, new Map(outputs)),
                    );
                    if (a.wait) await Promise.all(tasks);
                    // Observe detached preview tasks so their failures do not become unhandled rejections
                    else for (const task of tasks) void task.catch(() => {});
                    break;
                }
                case 'saved_macro': {
                    if (calls.has(a.macro_id)) throw Error('Saved macro reference cycle');
                    const saved = macros.find((m) => m.id === a.macro_id);
                    if (!saved) throw Error('Saved macro missing');
                    calls.add(a.macro_id);
                    await execute(saved.action, depth + 1, `${path}.macro`, outputs);
                    calls.delete(a.macro_id);
                    break;
                }
                case 'random_action':
                    await branch(a.children, 0);
                    break;
                case 'action_menu':
                    await branch(a.children, 0);
                    break;
                case 'cooldown':
                    if (Date.now() - (this.cooldowns.get(key) || 0) >= a.milliseconds) {
                        this.cooldowns.set(key, Date.now());
                        await branch(a.children, 0);
                    }
                    break;
                case 'reset_state':
                    this.indices.clear();
                    this.cooldowns.clear();
                    break;
                case 'cancel_macros':
                    this.generation++;
                    return;
                case 'pause_bindings':
                    if (a.operation === 'resume') this.paused.delete(device);
                    else if (a.operation === 'pause' || !this.paused.has(device))
                        this.paused.set(device, control);
                    else this.paused.delete(device);
                    break;
                case 'page_step': {
                    const profile = currentProfile();
                    if (!profile) throw Error('Profile missing');
                    const current = profile.pages.findIndex((p) => p.id === session()?.page_id);
                    const next = current + (a.direction === 'previous' ? -1 : 1);
                    const index = a.wrap
                        ? (next + profile.pages.length) % profile.pages.length
                        : next;
                    if (profile.pages[index]) {
                        const history = this.history.get(device) || [];
                        history.push(session()?.page_id || profile.active_page);
                        this.history.set(device, history);
                        select(profile.id, profile.pages[index].id, false);
                    }
                    break;
                }
                case 'page_back': {
                    const page = this.history.get(device)?.pop();
                    if (page && currentProfile()) select(currentProfile()!.id, page, false);
                    break;
                }
                case 'momentary_page': {
                    const profile = currentProfile();
                    if (!profile) throw Error('Profile missing');
                    const previous = session()?.page_id || profile.active_page;
                    select(profile.id, a.page_id, false);
                    await release();
                    if (session()?.page_id === a.page_id) select(profile.id, previous, false);
                    break;
                }
                case 'brightness_step': {
                    const current = session();
                    if (current)
                        current.output.brightness = Math.max(
                            0,
                            Math.min(100, current.output.brightness + a.delta),
                        );
                    break;
                }
                case 'automation_control': {
                    const current = session();
                    if (current) {
                        if (a.operation === 'disable') current.automation_enabled = false;
                        else if (a.operation === 'pause') current.automation_paused = true;
                        else {
                            current.automation_enabled =
                                a.operation === 'toggle' ? !current.automation_enabled : true;
                            current.automation_paused = false;
                        }
                    }
                    break;
                }
                case 'date_time':
                    effects.push('Date/time: 2000-01-01 00:00 (simulated)');
                    break;
                case 'notification':
                    effects.push(
                        `Notification: ${interpolate(a.title, a.template)} ${interpolate(a.text, a.template)}`,
                    );
                    break;
                case 'transform_text':
                    effects.push(`Transform selected text: ${a.operation}`);
                    break;
                case 'search_selection':
                    effects.push('Search selected text (simulated)');
                    break;
                case 'binding_layer':
                    effects.push('Temporary binding layer (simulated)');
                    if (!a.toggle) await release();
                    break;
                case 'trigger_binding': {
                    const target = snapshot.profiles
                        .find((p) => p.id === (a.profile_id || currentProfile()?.id))
                        ?.pages.find((p) => p.id === (a.page_id || session()?.page_id))?.bindings[
                        a.control_id
                    ];
                    if (!target) throw Error('Target binding missing');
                    await execute(target.action, depth + 1, `${path}.binding`, outputs);
                    break;
                }
                case 'keyboard_macro': {
                    const error = validateKeyboardMacro(a);
                    if (error) throw Error(error);
                    effects.push(`Macro: ${keyboardMacroSummary(a)} (simulated)`);
                    break;
                }
                case 'latch_keys':
                case 'mouse_click':
                case 'mouse_move':
                case 'mouse_scroll':
                case 'mouse_drag':
                case 'open_link':
                case 'reveal_file':
                case 'show_manager':
                case 'edit_binding':
                case 'open_with':
                case 'open_terminal':
                    effects.push(`${a.kind} (simulated)`);
                    break;
                default: {
                    const exhaustive: never = a;
                    throw Error(`Unknown preview action ${String(exhaustive)}`);
                }
            }
        };
        await execute(action);
        return `Preview completed${effects.length ? `: ${effects.slice(0, 40).join('; ').slice(0, 5000)}` : ''}`;
    }
}
