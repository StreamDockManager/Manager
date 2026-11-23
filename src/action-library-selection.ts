import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { windowWork } from './window-work';
import type { Action } from './types';

export type LibrarySelection =
    { kind: string } | { action: Extract<Action, { kind: 'plugin' }>; name: string };
export interface ActionLibraryRequest {
    owner: HTMLElement;
    current: string;
    advanced?: boolean;
    standalone?: boolean;
    multiAction?: boolean;
    keyLogic?: boolean;
    independentEvent?: boolean;
    source?: 'all' | 'plugins';
    label: string;
    valid?: () => boolean;
    canResume?: () => boolean;
    choose: (selection: LibrarySelection) => void;
}
let dependencies: { Alpine: typeof AlpineType; app: () => AppState };
// Read the current application state for the selection session
const app = () => dependencies.app();
// Capture the current device and profile selection context
const context = () =>
    JSON.stringify([app().deviceId, app().profile?.id, app().page?.id, app().controlId]);
// Notify route listeners after changing the selection route
const signalNavigation = () => window.dispatchEvent(new HashChangeEvent('hashchange'));

/** The retained editors are the draft snapshot: preserve object identity, invalid input,
 * alternative plugin settings, conflict baselines and macro node IDs during a detour */
class ActionLibrarySelection {
    request:
        | (ActionLibraryRequest & {
              id: string;
              route: string;
              hash: string;
              routeElement: HTMLElement | null;
              context: string;
              dialogs: HTMLDialogElement[];
              scroll: { element: Element; top: number; left: number }[];
              viewport: { x: number; y: number };
          })
        | null = null;
    phase: 'opening' | 'choosing' | 'returning' | 'suspended' = 'suspended';
    selected: LibrarySelection | null = null;
    filters = {
        favoritesOnly: false,
        search: '',
        category: 'all',
        source: 'all',
        page: 1,
        pageSize: '12',
    };
    // Report whether an action-library selection is in progress
    get choosing() {
        return Boolean(this.request && (this.phase === 'opening' || this.phase === 'choosing'));
    }
    // Check that the original selection context is still usable
    get valid() {
        const request = this.request;
        return Boolean(
            request?.owner.isConnected &&
            (request.standalone || request.context === context()) &&
            (!request.valid || request.valid()),
        );
    }
    // Recognize elements owned by the active selection flow
    owns(element: Element) {
        return Boolean(this.request && element.contains(this.request.owner));
    }
    // Open the action library for a scoped selection request
    open(request: ActionLibraryRequest) {
        if (!request.owner.isConnected || windowWork.quiescing || app().navigationPending) return;
        if (this.request) {
            app().notify('Return to your unfinished action selection first.', 'warning');
            return;
        }
        const dialogs = Array.from(document.querySelectorAll<HTMLDialogElement>('dialog:modal'));
        const scrollRegions = new Set<Element>(
            document.querySelectorAll(
                '#editor-outlet .workspace, #editor-outlet .inspector, dialog:modal .feature-body, dialog:modal .picker-content, dialog:modal .macro-tree, dialog:modal .macro-detail, dialog:modal .macro-debug-events, dialog:modal .macro-debug-output',
            ),
        );
        for (let node: Element | null = request.owner; node; node = node.parentElement)
            scrollRegions.add(node);
        this.request = {
            ...request,
            id: crypto.randomUUID(),
            route: app().route,
            hash: '#/' + app().route,
            routeElement: document.querySelector<HTMLElement>('#route-outlet > :not([hidden])'),
            context: context(),
            dialogs,
            scroll: Array.from(scrollRegions)
                .filter((element) => element.scrollTop || element.scrollLeft)
                .map((element) => ({ element, top: element.scrollTop, left: element.scrollLeft })),
            viewport: { x: scrollX, y: scrollY },
        };
        this.selected = null;
        this.phase = 'opening';
        this.filters = {
            ...this.filters,
            favoritesOnly: false,
            search: '',
            category: 'all',
            source: request.source || 'all',
            page: 1,
        };
        // Closing only the native surface keeps each controller's isolated draft alive
        for (const dialog of [...dialogs].reverse()) dialog.close();
        history.pushState(
            { ...history.state, actionLibrarySelection: this.request.id },
            '',
            '#/action-library',
        );
        signalNavigation();
    }
    // Deliver a selection to the original requesting editor
    choose(selection: LibrarySelection) {
        if (!this.choosing || !this.valid) return;
        this.selected = selection;
        this.returnToOrigin();
    }
    // Cancel the selection and return to its origin
    cancel() {
        this.selected = null;
        this.returnToOrigin();
    }
    // Restore the originating route and editor focus
    returnToOrigin() {
        if (!this.request) return;
        if (this.phase === 'returning') {
            if (!app().navigationPending) {
                history.replaceState(history.state, '', this.request.hash);
                signalNavigation();
            }
            return;
        }
        this.phase = 'returning';
        if (history.state?.actionLibrarySelection === this.request.id) history.back();
        else {
            history.replaceState(history.state, '', this.request.hash);
            signalNavigation();
        }
    }
    // Reconcile route navigation with the active selection request
    async navigated(route: string, current: () => boolean) {
        const request = this.request;
        if (!request) return;
        if (
            route === 'action-library' &&
            (this.phase === 'opening' || this.phase === 'suspended')
        ) {
            this.phase = 'choosing';
            await dependencies.Alpine.nextTick();
            if (current() && this.request === request)
                document
                    .querySelector<HTMLInputElement>('sd-action-library input[type="search"]')
                    ?.focus();
            return;
        }
        if (route !== request.route) {
            if (route !== 'action-library' || this.phase !== 'choosing') {
                this.phase = 'suspended';
                this.selected = null;
            }
            return;
        }
        await dependencies.Alpine.nextTick();
        if (!current() || this.request !== request) return;
        if (!this.valid) {
            this.selected = null;
            app().notify('The editing target changed. No action was applied.', 'warning');
            if (!request.canResume?.()) {
                this.request = null;
                return;
            }
        }
        const selection = this.selected;
        if (selection && 'action' in selection) {
            const action = selection.action;
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === action.plugin_id);
            const descriptor = plugin?.actions.find((item) => item.id === action.action_id);
            if (
                !plugin?.enabled ||
                !descriptor ||
                (!request.standalone &&
                    !(descriptor.control_types ?? ['button']).includes(app().control?.kind || ''))
            ) {
                this.selected = null;
                app().notify(
                    'This plugin action is no longer available. Your draft is unchanged.',
                    'warning',
                );
            }
        }
        if (this.selected) request.choose(this.selected);
        this.selected = null;
        await dependencies.Alpine.nextTick();
        if (!current() || this.request !== request) return;
        for (const dialog of request.dialogs)
            if (dialog.isConnected && !dialog.open) dialog.showModal();
        for (const position of request.scroll)
            if (position.element.isConnected) {
                position.element.scrollTop = position.top;
                position.element.scrollLeft = position.left;
            }
        window.scrollTo(request.viewport.x, request.viewport.y);
        const owner = request.owner.isConnected
            ? request.owner
            : request.dialogs.at(-1)?.querySelector<HTMLElement>('.action-chooser');
        this.request = null;
        // Restore focus after the destination view has rendered
        requestAnimationFrame(() => {
            if (current() && owner?.isConnected && owner.getClientRects().length)
                owner.focus({ preventScroll: true });
        });
    }
}
export let actionLibrarySelection = new ActionLibrarySelection();
// Register the shared action-library selection controller
export function registerActionLibrarySelection(Alpine: typeof AlpineType, app: () => AppState) {
    dependencies = { Alpine, app };
    actionLibrarySelection = Alpine.reactive(actionLibrarySelection);
}
