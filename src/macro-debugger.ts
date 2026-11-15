import type { Bridge } from './bridge';
import { trackCloseRelease } from './close-releases';
import type {
    DebugCommand,
    DebugMode,
    DebugStart,
    DebugStepEvent,
    DebugView,
} from '../generated/runtime';

// Browser tests supply these wire responses; actual simulation runs only in Rust
export class MacroDebugger {
    mode: DebugMode = 'simulation';
    inputValues = '{}';
    simulation = '{}';
    breakpoints: string[] = [];
    view: DebugView | null = null;
    events: DebugStepEvent[] = [];
    error = '';
    busy = false;
    epoch = 0;
    cursor = 0;
    mutation = 0;
    selectedPausedPath = '';
    selectedSequence = 0;
    truncated = false;
    // Select the recorded macro event shown in the debugger
    selectEvent(sequence: number) {
        this.selectedSequence = sequence;
    }
    // Return the action paths currently paused by the debugger
    get pausedPaths() {
        return this.view?.paused_paths.map((path) => path.join('.')) || [];
    }
    timer: ReturnType<typeof setTimeout> | undefined;
    bridge: Bridge | undefined;
    // Return active for the current macro debugger view
    get active() {
        return Boolean(this.view && this.view.status !== 'finished');
    }
    // Return dirty for the current macro debugger view
    get dirty() {
        return (
            this.active ||
            this.busy ||
            this.mode !== 'simulation' ||
            this.inputValues !== '{}' ||
            this.simulation !== '{}' ||
            this.breakpoints.length > 0
        );
    }
    // Check whether the paused debugger can advance one action
    get canStep() {
        return this.view?.status === 'paused' && !this.busy;
    }
    // Check whether the paused debugger can resume execution
    get canContinue() {
        return !this.busy && ['paused', 'pausing'].includes(this.view?.status || '');
    }
    // Return status for the current macro debugger view
    get status() {
        return this.view
            ? `${this.view.status}${this.view.outcome ? ` · ${this.view.outcome}` : ''}`
            : this.busy
              ? 'Starting…'
              : this.error
                ? 'Test unavailable'
                : 'Ready';
    }
    // Return output for the current macro debugger view
    get output() {
        const selected = this.events.find((event) => event.sequence === this.selectedSequence);
        return (
            (selected || [...this.events].reverse().find((event) => event.detail))?.detail ||
            'No output for this event.'
        );
    }
    // Apply the latest debug view without accepting an obsolete run
    receive(view: DebugView) {
        if (this.view && view.id !== this.view.id) return;
        if (view.cursor < this.cursor) return;
        const additions = view.events.filter((event) => event.sequence > this.cursor);
        if (additions[0]?.sequence > this.cursor + 1) this.events = [];
        const events = [...this.events, ...additions];
        this.truncated ||= view.truncated || events.length > 256;
        this.events = events.slice(-256);
        if (!this.events.some((event) => event.sequence === this.selectedSequence))
            this.selectedSequence = 0;
        this.cursor = Math.max(this.cursor, view.cursor);
        this.view = view;
        if (!this.pausedPaths.includes(this.selectedPausedPath))
            this.selectedPausedPath = this.pausedPaths[0] || '';
    }
    // Start a new debug run from the supplied action and execution options
    async start(
        bridge: Bridge,
        request: Omit<DebugStart, 'mode' | 'arguments' | 'simulation' | 'breakpoints'>,
    ) {
        if (this.active || this.busy) return;
        const epoch = ++this.epoch;
        this.clearResults();
        this.error = '';
        this.busy = true;
        this.bridge = bridge;
        try {
            const values = JSON.parse(this.inputValues) as Record<
                string,
                import('../generated/runtime').JsonValue
            >;
            if (!values || typeof values !== 'object' || Array.isArray(values))
                throw Error('Enter input values as a JSON object.');
            const simulation = JSON.parse(
                this.mode === 'simulation' ? this.simulation : '{}',
            ) as DebugStart['simulation'];
            if (!simulation || typeof simulation !== 'object' || Array.isArray(simulation))
                throw Error('Enter simulation values as a JSON object.');
            const view = await bridge.command('start_macro_debug', {
                request: {
                    ...request,
                    mode: this.mode,
                    arguments: Object.fromEntries(
                        Object.entries(values).map(([name, value]) => [
                            name,
                            { kind: 'literal', value },
                        ]),
                    ),
                    simulation,
                    breakpoints: this.breakpoints.map((path) =>
                        path === '' ? [] : path.split('.').map(Number),
                    ),
                },
            });
            if (epoch !== this.epoch) {
                await trackCloseRelease(`macro-debug:${view.id}`, () =>
                    bridge.command('macro_debug_command', { id: view.id, command: 'stop' }),
                );
                return;
            }
            this.receive(view);
            this.schedule(epoch);
        } catch (error) {
            if (epoch === this.epoch) this.error = String(error);
        } finally {
            if (epoch === this.epoch) this.busy = false;
        }
    }
    // Schedule the next debug poll for the current run generation
    schedule(epoch: number) {
        if (!this.active || epoch !== this.epoch) return;
        // Resume scheduled macro debugger work while its owner remains active
        this.timer = setTimeout(() => {
            void this.poll(epoch);
        }, 150);
    }
    // Refresh debugger state only while the requested run generation remains current
    async poll(epoch: number) {
        const id = this.view?.id;
        if (!id || epoch !== this.epoch) return;
        const mutation = this.mutation;
        try {
            const view = await this.bridge!.command('macro_debug_view', {
                id,
                after: this.cursor,
            });
            if (epoch === this.epoch && this.view?.id === id && mutation === this.mutation)
                this.receive(view);
        } catch (error) {
            if (epoch === this.epoch) {
                this.error = String(error);
                this.dispose();
            }
        }
        this.schedule(epoch);
    }
    // Send a debugger control command and refresh the resulting run state
    async command(command: DebugCommand) {
        const id = this.view?.id,
            epoch = this.epoch;
        if (!id || this.busy) return;
        this.busy = true;
        this.error = '';
        ++this.mutation;
        try {
            const view = await this.bridge!.command('macro_debug_command', {
                id,
                command,
                path:
                    command === 'step_into' || command === 'step_over'
                        ? this.selectedPausedPath === ''
                            ? []
                            : this.selectedPausedPath.split('.').map(Number)
                        : null,
            });
            if (epoch === this.epoch && this.view?.id === id) this.receive(view);
        } catch (error) {
            if (epoch === this.epoch) this.error = String(error);
        } finally {
            if (epoch === this.epoch) this.busy = false;
        }
    }
    // Invalidate the current debug generation and clear its visible state
    reset() {
        this.dispose();
        this.mode = 'simulation';
        this.inputValues = '{}';
        this.simulation = '{}';
        this.breakpoints = [];
        this.error = '';
        this.clearResults();
    }
    // Clear recorded debug output without changing the edited action
    clearResults() {
        this.view = null;
        this.events = [];
        this.cursor = 0;
        this.selectedSequence = 0;
        this.selectedPausedPath = '';
        this.truncated = false;
    }
    // Release debugger timers and the active run ownership
    dispose() {
        // Report the asynchronous macro debugger failure without applying stale results
        void this.release().catch(() => {});
    }
    // Release resources retained by the current controller
    async release() {
        ++this.epoch;
        if (this.timer) clearTimeout(this.timer);
        this.timer = undefined;
        const id = this.active ? this.view?.id : undefined;
        this.view = null;
        this.busy = false;
        const bridge = this.bridge;
        if (id && bridge)
            await trackCloseRelease(`macro-debug:${id}`, () =>
                bridge.command('macro_debug_command', { id, command: 'stop' }),
            );
    }
}
