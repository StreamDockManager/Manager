import type AlpineType from '@alpinejs/csp';
import type { Bridge } from './bridge';
import { native } from './bridge';
import { cloneJson } from './json-clone';
import { trackCloseRelease } from './close-releases';
import { windowCloseOwners } from './window-close-owners';
import {
    PHYSICAL_KEYS,
    addKeyboardCombo,
    isPhysicalKey,
    keyboardMacroGroups,
    keyboardMacroSummary,
    validateKeyboardMacro,
    type KeyboardMacroAction,
} from './keyboard-macro-draft';
import type {
    KeyboardMacroCapabilities,
    KeyboardMacroCaptureEvent,
    KeyboardMacroCaptureMode,
    KeyboardMacroSession,
    KeyboardMacroTestSession,
    PhysicalKey,
} from '../generated/runtime';
let openEditor:
    | ((
          value: KeyboardMacroAction,
          opener: HTMLElement,
          signal: AbortSignal,
      ) => Promise<KeyboardMacroAction | null>)
    | undefined;
// Request editing of the selected keyboard macro action
export function editKeyboardMacro(
    value: KeyboardMacroAction,
    opener: HTMLElement,
    signal: AbortSignal,
) {
    if (!openEditor) return Promise.reject(Error('The keyboard editor is not ready.'));
    return openEditor(value, opener, signal);
}
// Register keyboard macro editing, recording and test controls
export function registerKeyboardMacroEditor(Alpine: typeof AlpineType, bridge: Bridge) {
    Alpine.data('keyboardMacroEditor', () => {
        let resolve: ((value: KeyboardMacroAction | null) => void) | undefined;
        let opener: HTMLElement | undefined;
        let signal: AbortSignal | undefined;
        let generation = 0;
        let pending: KeyboardMacroCaptureEvent[] = [];
        let work = Promise.resolve();
        let started = 0;
        let pausedAt = 0;
        const heldKeys = new Set<PhysicalKey>();
        let timer: ReturnType<typeof setInterval> | undefined;
        let unregister: (() => void) | undefined;
        let abortListener: (() => void) | undefined;
        let beforeRecording: KeyboardMacroAction | null = null;
        let sessionMode: KeyboardMacroCaptureMode = 'in_app';
        let heartbeatSession: string | undefined;
        let testGeneration = 0;
        let testTimer: ReturnType<typeof setInterval> | undefined;
        let testBeginning: Promise<KeyboardMacroTestSession> | undefined;
        let testPolling: string | undefined;
        let testCanceling: Promise<boolean> | undefined;
        let testAction: KeyboardMacroAction | null = null;
        let testFailureRevision = 0;
        let closeAfterTest = false;
        type TestOutcome = KeyboardMacroTestSession | null;
        type TestRelease = {
            first?: Promise<TestOutcome>;
            resolve?: (value: TestOutcome) => void;
            reject?: (error: unknown) => void;
        };
        const testReleases = new Map<string, TestRelease>();
        // Recognize completed or stopped macro test states
        const terminalTest = (session: TestOutcome) =>
            !session || ['completed', 'canceled', 'failed'].includes(session.state);
        // Release the current macro test session through the bridge
        const releaseTest = (sessionId: string): Promise<TestOutcome> => {
            let entry = testReleases.get(sessionId);
            if (!entry) {
                entry = {};
                testReleases.set(sessionId, entry);
            }
            if (entry.first) return entry.first;
            const current = entry;
            const first = new Promise<TestOutcome>((resolve, reject) => {
                current.resolve = resolve;
                current.reject = reject;
            });
            current.first = first;
            // Return the initial native outcome promptly, but keep document-close
            // ownership until asynchronous native teardown reaches a terminal state
            void trackCloseRelease(`keyboard-test:${sessionId}`, async () => {
                let result = await bridge.command('cancel_keyboard_macro_test', { sessionId });
                if (result && result.state !== 'canceling' && !terminalTest(result))
                    throw Error('Unexpected test cancellation state');
                current.resolve?.(result);
                while (!terminalTest(result)) {
                    await new Promise((resolve) => setTimeout(resolve, 500));
                    result = await bridge.command('keyboard_macro_test_status', { sessionId });
                }
            }).then(
                () => {
                    testReleases.delete(sessionId);
                },
                (error) => {
                    current.first = undefined;
                    current.reject?.(error);
                },
            );
            return first;
        };
        return {
            draft: null as KeyboardMacroAction | null,
            capabilities: null as KeyboardMacroCapabilities | null,
            session: null as KeyboardMacroSession | null,
            busy: false,
            recording: false,
            paused: false,
            captureText: '',
            lastInput: '',
            recordingMode: 'in_app' as KeyboardMacroCaptureMode,
            countdownSeconds: 3,
            testSession: null as KeyboardMacroTestSession | null,
            testSetup: false,
            testBusy: false,
            testError: '',
            testNotice: '',
            error: '',
            notice: '',
            simulation: '',
            unsupported: [] as string[],
            confirmReplace: false,
            elapsed: 0,
            eventCount: 0,
            timelinePage: 0,
            manualKey: 'KeyA' as PhysicalKey,
            manualModifiers: [] as PhysicalKey[],
            manualDelay: 0,
            manualHold: 80,
            keyChoices: PHYSICAL_KEYS,
            modifierChoices: [
                'ControlLeft',
                'ControlRight',
                'ShiftLeft',
                'ShiftRight',
                'AltLeft',
                'AltRight',
                'MetaLeft',
                'MetaRight',
            ],
            // Resolve the keyboard macro editor dialog element
            get dialog(): HTMLDialogElement {
                return this.$refs.dialog as HTMLDialogElement;
            },
            // Resolve the recording-review dialog element
            get recordingDialog(): HTMLDialogElement {
                return this.$refs.recordingDialog as HTMLDialogElement;
            },
            // Resolve the focused DOM surface used for in-app keyboard capture
            get capture(): HTMLElement {
                return this.$refs.capture;
            },
            // Validate the current keyboard macro draft for display
            get validation() {
                return this.draft ? validateKeyboardMacro(this.draft) : '';
            },
            // Prevent draft edits while capture or playback owns the timeline
            get editingLocked() {
                return (
                    this.recording ||
                    this.busy ||
                    !!this.session ||
                    this.testBusy ||
                    !!this.testSession
                );
            },
            // Determine whether the current draft can be committed
            get canApply() {
                return (
                    !!this.draft &&
                    !this.validation &&
                    !this.unsupported.length &&
                    !this.editingLocked
                );
            },
            // Summarize the current configuration for display
            get summary() {
                return this.draft ? keyboardMacroSummary(this.draft) : '';
            },
            // Expose the current page of keyboard macro events
            get timeline() {
                return (this.draft?.recording.events || [])
                    .slice(this.timelinePage * 100, (this.timelinePage + 1) * 100)
                    .map((event, offset) => ({
                        ...event,
                        index: this.timelinePage * 100 + offset,
                    }));
            },
            // Calculate the number of macro timeline pages
            get pageCount() {
                return Math.max(1, Math.ceil((this.draft?.recording.events.length || 0) / 100));
            },
            // Check whether keyboard recording is available on this host
            get recordAvailable() {
                return (
                    (this.recordingMode === 'global'
                        ? !!this.capabilities?.global_recording && !this.repeatModeRequired
                        : !!this.capabilities?.in_app_recording) && !this.editingLocked
                );
            },
            // Determine whether playback requires an explicit repeat choice
            get repeatModeRequired() {
                return (
                    !!this.capabilities?.required_repeat_mode &&
                    this.draft?.playback.repeat_mode !== this.capabilities.required_repeat_mode
                );
            },
            // Describe the host-supported emergency stop shortcut
            get emergencyShortcut() {
                return this.recordingMode === 'global'
                    ? this.capabilities?.global_emergency_shortcut || 'Unavailable'
                    : this.capabilities?.emergency_shortcut || 'Ctrl+Shift+F12';
            },
            // Describe the current recording state for the user
            get recordingStatus() {
                if (!this.recording)
                    return this.busy ? 'Finishing recording…' : 'Recording interrupted';
                if (this.paused) return 'Recording paused';
                if (this.session?.state === 'waiting_for_release')
                    return 'Release held modifiers to begin.';
                if (this.session?.state === 'countdown')
                    return this.countdownSeconds > 0
                        ? `Starts in ${this.countdownSeconds} seconds. Switch to your scratch app.`
                        : 'Waiting for native capture to become ready…';
                return sessionMode === 'global'
                    ? 'Recording other applications'
                    : 'Recording in Manager';
            },
            // Explain playback behavior and restrictions for the current macro
            get playbackHelp() {
                if (this.repeatModeRequired)
                    return 'This backend records physical press/release timing, not application-generated repeats. Explicitly choose System repeat (Linux) before global recording or native playback. Exact mode is not changed automatically.';
                if (this.systemRepeat && this.capabilities?.platform !== 'linux')
                    return 'System repeat is available only on supported Linux backends. You can save this action for Linux, or choose Exact recorded repeats to test here. Simulate sends no keys.';
                return (
                    this.capabilities?.test_reason ||
                    (this.capabilities?.test_playback
                        ? 'Test sends keys only to a scratch target you explicitly choose. Simulate sends no keys.'
                        : 'Test playback is unavailable until native safety checks and scratch-target selection are ready. Simulate sends no keys.')
                );
            },
            // Check whether this host permits keyboard macro tests
            get testAvailable() {
                return (
                    !!this.capabilities?.test_playback &&
                    !this.repeatModeRequired &&
                    (!this.systemRepeat || this.capabilities.platform === 'linux') &&
                    this.canApply
                );
            },
            // Determine whether the selected mode uses system key repeat
            get systemRepeat() {
                return this.draft?.playback.repeat_mode === 'system';
            },
            // Require a valid macro and ready target before starting a test
            get canStartTest() {
                return (
                    this.testSession?.state === 'target_ready' &&
                    !!this.testSession.target_label &&
                    !this.testBusy &&
                    !this.testError
                );
            },
            // Describe the current macro test result or progress
            get testStatus() {
                const seconds = Math.ceil((this.testSession?.remaining_ms || 0) / 1000);
                switch (this.testSession?.state) {
                    case 'selecting_target':
                        return `Choosing target in ${seconds} seconds. Switch to your blank scratch window.`;
                    case 'target_ready':
                        return 'Scratch target selected. Confirm before sending any keys.';
                    case 'countdown':
                        return `Test starts in ${seconds} seconds. Switch back to the selected scratch window.`;
                    case 'running':
                        return `Test running · ${this.testSession.event_count} ${this.systemRepeat ? 'recorded event checkpoints' : 'input events'}.`;
                    case 'canceling':
                        return 'Canceling test. Waiting for native key release…';
                    case 'completed':
                        return 'Test completed. Releasing the test session…';
                    case 'canceled':
                        return 'Test canceled. Releasing the test session…';
                    case 'failed':
                        return 'Test stopped. Releasing the test session…';
                    default:
                        return this.testBusy ? 'Preparing scratch-target selection…' : '';
                }
            },
            // Expose the host-specific recording or playback limitation
            get platformWarning() {
                return this.draft?.recording.platform === 'unknown'
                    ? 'Platform is unspecified. Physical positions and shortcuts may differ on another keyboard or platform.'
                    : `Recorded for ${this.draft?.recording.platform}. Physical positions are preserved; shortcuts are not translated across platforms.`;
            },
            // Attach keyboard macro editing listeners and window-close coordination
            init() {
                // Cancel the active recording when capture loses its authority
                const abort = () => {
                    void this.cancel();
                };
                abortListener = abort;
                // Reject overlapping or already-aborted keyboard macro editing requests
                openEditor = async (value, target, ownerSignal) => {
                    if (this.draft) throw Error('A keyboard recording is already being edited.');
                    if (ownerSignal.aborted) return null;
                    const ownGeneration = ++generation;
                    opener = target;
                    signal = ownerSignal;
                    signal.addEventListener('abort', abort, { once: true });
                    this.draft = cloneJson(value);
                    // Older saved actions predate repeat_mode and retain exact semantics
                    if (this.draft.playback.repeat_mode === undefined)
                        this.draft.playback.repeat_mode = 'exact';
                    this.error = this.notice = this.simulation = '';
                    this.unsupported = [];
                    this.capabilities = null;
                    this.timelinePage = 0;
                    this.confirmReplace = false;
                    this.recordingMode = 'in_app';
                    this.testSetup = false;
                    this.testError = this.testNotice = '';
                    closeAfterTest = false;
                    const result = new Promise<KeyboardMacroAction | null>((done) => {
                        resolve = done;
                    });
                    this.dialog.showModal();
                    void bridge
                        .command('keyboard_macro_capabilities')
                        // Apply host capabilities only to the editor generation that requested them
                        .then((value) => {
                            if (generation === ownGeneration && this.draft)
                                this.capabilities = value;
                        })
                        // Show capability failures only while the requesting editor remains active
                        .catch(() => {
                            if (generation === ownGeneration && this.draft)
                                this.error =
                                    'Recording capabilities could not be checked. Manual editing remains available.';
                        });
                    return result;
                };
                // Stop capture when the owning window loses focus
                const blur = () => this.blurCapture();
                // Stop capture when the document becomes hidden
                const hidden = () => {
                    if (document.hidden) this.blurCapture();
                };
                window.addEventListener('blur', blur);
                document.addEventListener('visibilitychange', hidden);
                window.addEventListener('hashchange', abort);
                window.addEventListener('pagehide', abort);
                // Warn when workspace changes invalidate assumptions in the open draft
                this.$watch('$store.app.configurationRevision', () => {
                    if (this.draft)
                        this.notice =
                            'The workspace changed. Apply updates only this draft; the parent save will check for conflicts.';
                });
                unregister = windowCloseOwners.register({
                    name: 'Keyboard recording draft',
                    // Report unsaved keyboard macro editing work to window-close coordination
                    state: () => (this.draft ? 'dirty' : 'clean'),
                    // Discard the current keyboard macro editing draft and release temporary state
                    discard: () => this.cancel(),
                    // Release temporary keyboard macro editing state during close coordination
                    release: () => this.cancel(),
                });
                // Detach global capture listeners when the editor owner releases them
                this.cleanup = () => {
                    window.removeEventListener('blur', blur);
                    document.removeEventListener('visibilitychange', hidden);
                    window.removeEventListener('hashchange', abort);
                    window.removeEventListener('pagehide', abort);
                    signal?.removeEventListener('abort', abort);
                };
            },
            // Release capture listeners, timers and active test sessions
            cleanup() {},
            // Complete the current macro workflow and release its resources
            async finish(value: KeyboardMacroAction | null) {
                ++generation;
                this.clearTimer();
                this.clearTestTimer();
                if (abortListener) signal?.removeEventListener('abort', abortListener);
                signal = undefined;
                this.closeRecordingDialog();
                this.dialog.close();
                this.draft = null;
                beforeRecording = null;
                pending = [];
                this.unsupported = [];
                this.recording = false;
                this.busy = false;
                this.session = null;
                this.testSession = null;
                this.testBusy = this.testSetup = false;
                testAction = null;
                this.confirmReplace = false;
                const done = resolve;
                resolve = undefined;
                done?.(value);
                if (!windowCloseOwners.closing && opener?.isConnected)
                    opener.focus({ preventScroll: true });
                opener = undefined;
            },
            // Discard the current macro editing operation
            async cancel() {
                if (!this.draft && !this.session && !this.testSession && !testBeginning) return;
                const acknowledgedFailure = testFailureRevision;
                ++generation;
                if (!(await this.cancelTest(false))) {
                    closeAfterTest = this.testSession?.state === 'canceling';
                    return;
                }
                if (testFailureRevision !== acknowledgedFailure) {
                    closeAfterTest = false;
                    return;
                }
                this.recording = false;
                this.clearTimer();
                pending = [];
                const session = this.session;
                if (session) {
                    try {
                        await trackCloseRelease(`keyboard-recording:${session.session_id}`, () =>
                            bridge.command('cancel_keyboard_macro_recording', {
                                sessionId: session.session_id,
                            }),
                        );
                    } catch {
                        this.error =
                            'Recording cancellation could not be confirmed. Close Manager or retry Cancel.';
                        return;
                    }
                }
                await this.finish(null);
            },
            // Accept the validated macro draft and complete the editing request
            async apply() {
                if (this.canApply) await this.finish(cloneJson(this.draft!));
            },
            // Close the macro editor without applying its draft
            dismiss(event: Event) {
                event.preventDefault();
                if (!this.recording) void this.cancel();
            },
            // Prepare a keyboard macro test and request any required target selection
            requestTest() {
                if (!this.testAvailable) return;
                this.testSetup = true;
                this.testError = this.testNotice = '';
            },
            // Select and validate the destination for macro testing
            async chooseTestTarget() {
                if (!this.testSetup || !this.testAvailable || !this.draft) return;
                const ownGeneration = generation;
                const ownTestGeneration = ++testGeneration;
                testAction = cloneJson(this.draft);
                this.testBusy = true;
                this.testError = this.testNotice = '';
                const beginning = bridge.command('begin_keyboard_macro_test', {
                    editorId: crypto.randomUUID(),
                });
                testBeginning = beginning;
                try {
                    const session = await beginning;
                    if (
                        generation !== ownGeneration ||
                        testGeneration !== ownTestGeneration ||
                        !this.draft
                    ) {
                        await releaseTest(session.session_id);
                        return;
                    }
                    this.testSession = session;
                    this.clearTestTimer();
                    testTimer = setInterval(() => {
                        void this.pollTest();
                    }, 500);
                } catch {
                    if (generation === ownGeneration && testGeneration === ownTestGeneration)
                        this.testError =
                            'Scratch-target selection could not start. Check native permissions and close other recording or playback sessions.';
                } finally {
                    if (testBeginning === beginning) testBeginning = undefined;
                    if (generation === ownGeneration && testGeneration === ownTestGeneration)
                        this.testBusy = false;
                }
            },
            // Start playback against the prepared macro test target
            async startTest() {
                if (!this.canStartTest || !this.testSession || !testAction) return;
                const ownGeneration = generation;
                const ownTestGeneration = ++testGeneration;
                const sessionId = this.testSession.session_id;
                this.testBusy = true;
                this.testError = '';
                try {
                    const session = await bridge.command('start_keyboard_macro_test', {
                        sessionId,
                        recording: cloneJson(testAction.recording),
                        playback: { ...testAction.playback, focus_policy: 'stay_on_target' },
                    });
                    if (generation !== ownGeneration || testGeneration !== ownTestGeneration) {
                        await releaseTest(sessionId);
                        return;
                    }
                    this.testSession = session;
                } catch {
                    if (generation === ownGeneration && testGeneration === ownTestGeneration) {
                        this.testError = 'Test could not start. Choose the scratch target again.';
                        // A transport failure cannot prove whether the native countdown started
                        await this.cancelTest(false);
                    }
                } finally {
                    if (generation === ownGeneration && testGeneration === ownTestGeneration)
                        this.testBusy = false;
                }
            },
            // Refresh the current macro test status while its session remains current
            async pollTest() {
                if (
                    !this.testSession ||
                    this.testBusy ||
                    testPolling === this.testSession.session_id
                )
                    return;
                const sessionId = this.testSession.session_id;
                const ownGeneration = generation;
                const ownTestGeneration = testGeneration;
                testPolling = sessionId;
                try {
                    const session = await bridge.command('keyboard_macro_test_status', {
                        sessionId,
                    });
                    if (
                        generation !== ownGeneration ||
                        testGeneration !== ownTestGeneration ||
                        this.testSession?.session_id !== sessionId
                    )
                        return;
                    this.testSession = session;
                    if (terminalTest(session)) {
                        // A terminal native snapshot is already confirmation that teardown
                        // finished. A later idempotent cache-release transport failure must
                        // not turn an ordinary Failed result into uncertain cancellation
                        void releaseTest(sessionId).catch(() => {});
                        this.finishTestOutcome(session);
                        if (closeAfterTest) {
                            closeAfterTest = false;
                            // A delayed teardown failure must remain visible. The next
                            // explicit Cancel acknowledges it and can close the editor
                            if (session.state !== 'failed') await this.cancel();
                        }
                    }
                } catch {
                    if (
                        generation === ownGeneration &&
                        testGeneration === ownTestGeneration &&
                        this.testSession?.session_id === sessionId
                    ) {
                        const pendingMessage =
                            'Test status was lost. Waiting for native cancellation.';
                        this.testError = pendingMessage;
                        if ((await this.cancelTest(false)) && this.testError === pendingMessage)
                            this.testError =
                                'Test status was lost. The native test session has ended; choose the scratch target again.';
                    }
                } finally {
                    if (testPolling === sessionId) testPolling = undefined;
                }
            },
            // Cancel the pending macro test status poll
            clearTestTimer() {
                if (testTimer) clearInterval(testTimer);
                testTimer = undefined;
            },
            // Apply a terminal macro test outcome to the editor state
            finishTestOutcome(session: TestOutcome, showNotice = true) {
                this.clearTestTimer();
                this.testSession = null;
                this.testSetup = false;
                testAction = null;
                if (session?.state === 'failed') {
                    ++testFailureRevision;
                    this.testNotice = '';
                    this.testError =
                        session.error ||
                        'Test stopped because its safety checks failed. Choose the scratch target again.';
                } else if (showNotice) {
                    this.testError = '';
                    this.testNotice =
                        session?.state === 'completed'
                            ? 'Test completed. The recording draft is unchanged.'
                            : 'Test canceled. The recording draft is unchanged.';
                }
            },
            // Stop the active macro test and release its session
            cancelTest(showNotice = true): Promise<boolean> {
                if (testCanceling) return testCanceling;
                // Await native test cancellation before releasing playback ownership
                const run = async (): Promise<boolean> => {
                    ++testGeneration;
                    this.clearTestTimer();
                    const beginning = testBeginning;
                    this.testBusy = !!beginning || !!this.testSession;
                    let session = this.testSession;
                    try {
                        if (!session && beginning) session = await beginning.catch(() => null);
                        if (session) {
                            const outcome = await releaseTest(session.session_id);
                            if (outcome?.state === 'canceling') {
                                this.testSession = outcome;
                                this.testSetup = false;
                                this.testNotice = '';
                                this.testError = outcome.error || '';
                                testTimer = setInterval(() => {
                                    void this.pollTest();
                                }, 500);
                                return false;
                            }
                            this.finishTestOutcome(outcome, showNotice);
                        } else {
                            this.finishTestOutcome(null, false);
                        }
                        return true;
                    } catch {
                        if (session) this.testSession = session;
                        this.testError =
                            'Test cancellation could not be confirmed. Retry Cancel test before editing or closing.';
                        return false;
                    } finally {
                        this.testBusy = false;
                    }
                };
                const canceling = run();
                testCanceling = canceling;
                void canceling.finally(() => {
                    if (testCanceling === canceling) testCanceling = undefined;
                });
                return canceling;
            },
            // Open recording controls after checking host capabilities
            requestRecord(event: Event) {
                if (!this.recordAvailable) return;
                if (this.draft?.recording.events.length) this.confirmReplace = true;
                else void this.start(event);
            },
            // Transfer the reviewed recording into the macro draft
            keepRecording() {
                this.confirmReplace = false;
            },
            // Start the keyboard recording session with the selected capture mode
            async start(event?: Event) {
                if (!this.recordAvailable || !this.draft) return;
                const ownGeneration = generation;
                beforeRecording = cloneJson(this.draft);
                this.confirmReplace = false;
                this.busy = true;
                this.error = this.notice = this.simulation = '';
                this.unsupported = [];
                pending = [];
                heldKeys.clear();
                this.paused = false;
                this.captureText = '';
                this.lastInput = '';
                this.eventCount = this.elapsed = 0;
                this.countdownSeconds = 3;
                sessionMode = this.recordingMode;
                const initiallyHeld: PhysicalKey[] = [];
                const click = event as MouseEvent | undefined;
                // Global capture owns physical-state preflight; browser flags have no side
                if (sessionMode === 'in_app') {
                    if (click?.ctrlKey) initiallyHeld.push('ControlLeft', 'ControlRight');
                    if (click?.shiftKey) initiallyHeld.push('ShiftLeft', 'ShiftRight');
                    if (click?.altKey) initiallyHeld.push('AltLeft', 'AltRight');
                    if (click?.metaKey) initiallyHeld.push('MetaLeft', 'MetaRight');
                }
                try {
                    const session = await bridge.command('start_keyboard_macro_recording', {
                        editorId: crypto.randomUUID(),
                        mode: sessionMode,
                        initiallyHeld,
                    });
                    if (generation !== ownGeneration || !this.draft) {
                        await trackCloseRelease(`keyboard-recording:${session.session_id}`, () =>
                            bridge.command('cancel_keyboard_macro_recording', {
                                sessionId: session.session_id,
                            }),
                        );
                        return;
                    }
                    this.session = session;
                    started = performance.now();
                    this.recording = true;
                    this.busy = false;
                    if (sessionMode === 'in_app') {
                        this.recordingDialog.showModal();
                        this.$nextTick(() => this.capture.focus());
                    }
                    let ticks = 0;
                    // Keep capture processing scoped to an active permitted recording mode
                    timer = setInterval(() => {
                        if (!this.recording) return;
                        if (sessionMode === 'in_app' && !this.paused) {
                            this.elapsed = Math.floor(performance.now() - started);
                            void this.flush();
                            if (this.elapsed >= (this.capabilities?.max_duration_ms || 300000))
                                void this.stop();
                        } else {
                            this.countdownSeconds = Math.max(
                                0,
                                Math.ceil((3000 - (performance.now() - started)) / 1000),
                            );
                        }
                        if (++ticks % 20 === 0) void this.heartbeat();
                    }, 50);
                } catch {
                    if (generation === ownGeneration)
                        this.error =
                            'Recording could not start. Check input permissions and close any other recording or playback.';
                } finally {
                    if (generation === ownGeneration) this.busy = false;
                }
            },
            // Forward a permitted physical key event to the current recording session
            captureKey(event: KeyboardEvent) {
                if (
                    !this.recording ||
                    this.paused ||
                    sessionMode !== 'in_app' ||
                    !this.session ||
                    !event.isTrusted ||
                    event.target !== this.capture ||
                    document.activeElement !== this.capture
                )
                    return;
                event.preventDefault();
                event.stopImmediatePropagation();
                if (
                    event.isComposing ||
                    event.key === 'Dead' ||
                    event.key === 'Process' ||
                    event.getModifierState('AltGraph') ||
                    !isPhysicalKey(event.code)
                ) {
                    const label =
                        event.isComposing || event.key === 'Process'
                            ? 'IME composition'
                            : event.key === 'Dead'
                              ? 'Dead key'
                              : event.getModifierState('AltGraph')
                                ? 'AltGr'
                                : event.code || 'Unknown key';
                    if (!this.unsupported.includes(label)) this.unsupported.push(label);
                    return;
                }
                if (this.eventCount >= (this.capabilities?.max_events || 10000)) {
                    void this.stop();
                    return;
                }
                if (
                    this.session.state !== 'waiting_for_release' &&
                    !heldKeys.has(event.code) &&
                    (event.type === 'keyup' || event.repeat)
                )
                    return;
                this.lastInput = `${event.code} · ${event.type === 'keyup' ? 'Release' : event.repeat ? 'Repeat' : 'Press'}`;
                if (event.type === 'keyup') heldKeys.delete(event.code);
                else heldKeys.add(event.code);
                // Scratch text is transient feedback, never saved with the macro
                if (event.type === 'keydown' && !event.ctrlKey && !event.metaKey && !event.altKey) {
                    if (event.key.length === 1) this.captureText += event.key;
                    else if (event.key === 'Enter') this.captureText += '\n';
                    else if (event.key === 'Backspace')
                        this.captureText = Array.from(this.captureText).slice(0, -1).join('');
                    this.captureText = this.captureText.slice(-10000);
                }
                pending.push({
                    kind: event.type === 'keyup' ? 'key_up' : event.repeat ? 'repeat' : 'key_down',
                    key: event.code,
                    elapsed_ms: Math.min(300000, Math.floor(performance.now() - started)),
                });
                // Mouse modifier flags have no side. While waiting, a false family flag
                // proves both sides are released; normalize only this ignored initial state
                if (this.session.state === 'waiting_for_release' && event.type === 'keyup') {
                    const modifiers = [
                        ['Control', event.ctrlKey],
                        ['Shift', event.shiftKey],
                        ['Alt', event.altKey],
                        ['Meta', event.metaKey],
                    ] as const;
                    for (const [family, held] of modifiers) {
                        if (!held && event.code.startsWith(family))
                            pending.push({
                                kind: 'key_up',
                                key: (family +
                                    (event.code.endsWith('Left')
                                        ? 'Right'
                                        : 'Left')) as PhysicalKey,
                                elapsed_ms: pending.at(-1)!.elapsed_ms,
                            });
                    }
                }
                this.eventCount++;
                if (
                    event.code === 'F12' &&
                    event.ctrlKey &&
                    event.shiftKey &&
                    event.type === 'keydown'
                )
                    void this.stop();
            },
            // Send the pending recorded key events to the host
            flush() {
                if (sessionMode !== 'in_app' || !pending.length || !this.session) return work;
                const events = pending.splice(0, 256);
                const sessionId = this.session.session_id;
                const ownGeneration = generation;
                work = work
                    // Append captured events only for the current recording generation
                    .then(async () => {
                        if (generation !== ownGeneration) return;
                        const session = await bridge.command('append_keyboard_macro_events', {
                            sessionId,
                            events,
                        });
                        if (
                            generation === ownGeneration &&
                            this.session?.session_id === sessionId
                        ) {
                            this.session = session;
                            if (session.state === 'failed') throw Error('Recording interrupted.');
                            if (session.state === 'stopped')
                                this.queueStoppedReview(sessionId, ownGeneration);
                        }
                    })
                    // Report recording failure only to the session that initiated the request
                    .catch(() => {
                        if (generation === ownGeneration) {
                            this.error =
                                'Recording interrupted or expired. Cancel this take and try again.';
                            this.recording = false;
                            this.clearTimer();
                        }
                    });
                return work;
            },
            // Keep the active recording lease alive while capture remains authorized
            async heartbeat() {
                if (
                    !this.session ||
                    !this.recording ||
                    heartbeatSession === this.session.session_id
                )
                    return;
                const sessionId = this.session.session_id;
                const ownGeneration = generation;
                heartbeatSession = sessionId;
                try {
                    const session = await bridge.command('heartbeat_keyboard_macro_recording', {
                        sessionId,
                    });
                    if (generation !== ownGeneration || this.session?.session_id !== sessionId)
                        return;
                    this.session = session;
                    if (sessionMode === 'global') {
                        this.eventCount = session.event_count;
                        this.elapsed = session.duration_ms;
                    }
                    if (session.state === 'failed' || session.state === 'canceled')
                        throw Error('Recording expired');
                    if (session.state === 'stopped')
                        this.queueStoppedReview(sessionId, ownGeneration);
                } catch {
                    if (generation === ownGeneration && this.session?.session_id === sessionId) {
                        this.error =
                            'The recording session expired. Cancel this take and try again.';
                        this.recording = false;
                        this.clearTimer();
                    }
                } finally {
                    if (heartbeatSession === sessionId) heartbeatSession = undefined;
                }
            },
            // Schedule review of the recording after capture stops
            queueStoppedReview(sessionId: string, ownGeneration: number) {
                // Do not await Stop from append's promise: Stop must drain that same work
                queueMicrotask(() => {
                    if (
                        generation === ownGeneration &&
                        this.session?.session_id === sessionId &&
                        this.recording &&
                        !this.busy
                    )
                        void this.stop();
                });
            },
            // Stop keyboard capture and retain the result for review
            async stop() {
                if (!this.recording || !this.session || this.busy) return;
                this.recording = false;
                this.busy = true;
                this.clearTimer();
                const ownGeneration = generation;
                const sessionId = this.session.session_id;
                try {
                    do {
                        await this.flush();
                    } while (pending.length);
                    if (this.error) return;
                    const result = await bridge.command('stop_keyboard_macro_recording', {
                        sessionId,
                    });
                    if (generation !== ownGeneration || !this.draft) return;
                    this.draft.recording = result.recording;
                    // Stop returns a copy; erase the backend review cache immediately
                    await trackCloseRelease(`keyboard-recording:${sessionId}`, () =>
                        bridge.command('cancel_keyboard_macro_recording', { sessionId }),
                    );
                    if (generation !== ownGeneration || !this.draft) return;
                    this.session = null;
                    this.closeRecordingDialog();
                    beforeRecording = null;
                    this.timelinePage = 0;
                    this.notice = result.session.balanced_releases
                        ? `Added ${result.session.balanced_releases} release events for keys held when recording stopped.`
                        : 'Recording stopped. Review before applying.';
                } catch {
                    if (generation === ownGeneration)
                        this.error =
                            'Recording could not be stopped safely. Cancel this take and try again.';
                } finally {
                    if (generation === ownGeneration) this.busy = false;
                }
            },
            // Close the recording dialog after its current operation completes
            closeRecordingDialog() {
                this.paused = false;
                this.captureText = '';
                this.lastInput = '';
                heldKeys.clear();
                this.recordingDialog.close();
            },
            // Discard the pending recording review
            dismissRecording(event: Event) {
                event.preventDefault();
                event.stopPropagation();
                void this.cancelTake();
            },
            // Pause or resume event capture within the active recording session
            togglePause(event?: MouseEvent) {
                if (!this.recording || this.busy || sessionMode !== 'in_app') return;
                if (this.paused) {
                    if (event?.ctrlKey || event?.shiftKey || event?.altKey || event?.metaKey) {
                        this.error = 'Release held modifiers before resuming.';
                        return;
                    }
                    this.error = '';
                    started += performance.now() - pausedAt;
                    this.paused = false;
                    this.$nextTick(() => this.capture.focus());
                } else {
                    pausedAt = performance.now();
                    this.elapsed = Math.floor(pausedAt - started);
                    this.paused = true;
                    // Close any partial chord before pausing so resume cannot inherit a
                    // modifier released while capture was suspended
                    for (const key of heldKeys) {
                        pending.push({ kind: 'key_up', key, elapsed_ms: this.elapsed });
                        this.eventCount++;
                    }
                    heldKeys.clear();
                    void this.flush();
                }
            },
            // Stop capture when the recording surface loses focus
            blurCapture() {
                if (this.recording && !this.paused && sessionMode === 'in_app') void this.stop();
            },
            // Cancel the pending recording heartbeat timer
            clearTimer() {
                if (timer) clearInterval(timer);
                timer = undefined;
            },
            // Cancel the current recording without applying captured events
            async cancelTake() {
                if (!this.session) return;
                const ownGeneration = ++generation;
                this.recording = false;
                this.clearTimer();
                pending = [];
                const sessionId = this.session.session_id;
                this.busy = true;
                try {
                    await trackCloseRelease(`keyboard-recording:${sessionId}`, () =>
                        bridge.command('cancel_keyboard_macro_recording', { sessionId }),
                    );
                    if (generation !== ownGeneration) return;
                    this.session = null;
                    this.closeRecordingDialog();
                    if (beforeRecording) this.draft = cloneJson(beforeRecording);
                    this.unsupported = [];
                    this.error = '';
                    this.notice = 'Recording canceled. The previous draft is unchanged.';
                } catch {
                    if (generation === ownGeneration)
                        this.error =
                            'Recording cancellation could not be confirmed. Retry Cancel recording.';
                } finally {
                    if (generation === ownGeneration) this.busy = false;
                }
            },
            // Remove unsupported events from the draft timeline
            removeUnsupported(event: Event) {
                this.unsupported.splice(
                    Number((event.currentTarget as HTMLElement).dataset.index),
                    1,
                );
            },
            // Add a physical-key event to the editable timeline
            addKey() {
                if (!this.draft || this.editingLocked) return;
                try {
                    addKeyboardCombo(
                        this.draft,
                        [...this.manualModifiers, this.manualKey],
                        Number(this.manualDelay),
                        Number(this.manualHold),
                    );
                    this.error = this.simulation = '';
                } catch (error) {
                    this.error = String(error);
                }
            },
            // Update the selected event in the keyboard macro draft
            editEvent(event: Event) {
                if (!this.draft || this.editingLocked) return;
                const input = event.currentTarget as HTMLInputElement;
                const item = this.draft.recording.events[Number(input.dataset.index)];
                if (!item) return;
                if (input.dataset.field === 'delay_ms') item.delay_ms = Number(input.value || NaN);
                else if (input.dataset.field === 'key' && isPhysicalKey(input.value))
                    item.key = input.value;
                else if (
                    input.dataset.field === 'kind' &&
                    ['key_down', 'key_up', 'repeat'].includes(input.value)
                )
                    item.kind = input.value as typeof item.kind;
                this.simulation = '';
            },
            // Move a timeline event while maintaining draft validation
            moveEvent(event: Event) {
                if (!this.draft || this.editingLocked) return;
                const button = event.currentTarget as HTMLElement;
                const index = Number(button.dataset.index);
                const events = this.draft.recording.events;
                if (button.dataset.op === 'remove') events.splice(index, 1);
                else {
                    const next = index + (button.dataset.op === 'up' ? -1 : 1);
                    if (next >= 0 && next < events.length)
                        [events[index], events[next]] = [events[next], events[index]];
                }
                this.timelinePage = Math.min(this.timelinePage, this.pageCount - 1);
                this.simulation = '';
            },
            // Return the timeline viewport to the current page start
            resetTimelineScroll() {
                this.$nextTick(() => {
                    // The timeline lives inside x-if; resolve its current mounted element,
                    // rather than relying on a ref from the conditional child scope
                    const timeline = this.dialog.querySelector<HTMLElement>(
                        '.keyboard-macro-event-scroll',
                    );
                    if (timeline) timeline.scrollTop = 0;
                });
            },
            // Move to the previous macro timeline page
            previousPage() {
                this.timelinePage = Math.max(0, this.timelinePage - 1);
                this.resetTimelineScroll();
            },
            // Move to the next macro timeline page
            nextPage() {
                this.timelinePage = Math.min(this.pageCount - 1, this.timelinePage + 1);
                this.resetTimelineScroll();
            },
            // Preview the macro timeline without sending physical input
            simulate() {
                if (this.canApply)
                    this.simulation = `Simulation only: ${this.summary}. ${keyboardMacroGroups(this.draft!.recording.events).length} balanced groups. No keys were sent.${this.systemRepeat ? ' The system-generated repeat count cannot be predicted.' : ''}`;
            },
            // Refresh the visual preview of the current macro draft
            get preview() {
                return !native;
            },
            // Detach keyboard macro editing listeners and release owned state
            destroy() {
                this.cleanup();
                unregister?.();
                openEditor = undefined;
                void this.cancel();
            },
        };
    });
}
