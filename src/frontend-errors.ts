import { invoke, isTauri } from '@tauri-apps/api/core';
import type {
    FailureReason,
    FrontendComponent,
    FrontendDiagnostic,
    FrontendEvent,
    FrontendFrame,
    FrontendStartupContext,
    StartupLoaderCode,
    ResourceContext,
} from '../generated/runtime';

const modules: [string, FrontendComponent][] = [
    ['route-diagnostics', 'route-diagnostics'],
    ['route-settings', 'route-settings'],
    ['route-plugins', 'route-plugins'],
    ['route-editor', 'route-editor'],
    ['route-shell', 'route-shell'],
    ['plugin-logs', 'plugin-logs'],
    ['pluginLogs', 'plugin-logs'],
    ['action-popup', 'action-popup'],
    ['actionPopup', 'action-popup'],
    ['main', 'main'],
];
// Classify a script URL into a known frontend component without exposing its path
function component(source: string, base: string): FrontendComponent {
    try {
        const url = new URL(source, base);
        if (url.origin !== new URL(base).origin || url.protocol === 'file:') return 'external';
        const path = url.pathname;
        if (path.includes('alpine-component-loader')) return 'acl';
        if (path.includes('alpinejs')) return 'alpine';
        const filename = path.split('/').at(-1) || '';
        return (
            modules.find(
                ([name]) =>
                    filename === `${name}.ts` ||
                    filename === `${name}.js` ||
                    filename.startsWith(`${name}-`),
            )?.[1] || 'app-script'
        );
    } catch {
        return 'unknown';
    }
}
// Clamp a finite diagnostic location to the supported nonnegative range
function number(value: unknown) {
    return typeof value === 'number' && Number.isFinite(value)
        ? Math.max(0, Math.min(10_000_000, Math.floor(value)))
        : 0;
}
// Classify an error message and name into a bounded failure category
function reason(text: string, name: string): FailureReason {
    text = text.toLowerCase();
    if (
        text.includes('dynamically imported') ||
        text.includes('loading module') ||
        text.includes('module script')
    )
        return 'module_load';
    if (
        text.includes('failed to fetch') ||
        text.includes('networkerror') ||
        text.includes('network request')
    )
        return 'network';
    if (text.includes('timed out') || text.includes('timeout')) return 'timeout';
    if (text.includes('permission denied') || text.includes('not allowed'))
        return 'permission_denied';
    if (name === 'TypeError') return 'type';
    if (name === 'ReferenceError') return 'reference';
    if (name === 'SyntaxError') return 'syntax';
    if (name === 'RangeError') return 'range';
    return 'unknown';
}

/** No messages, arbitrary object values, URLs, function names, or paths survive this projection */
export function frontendDiagnostic(
    event: FrontendEvent,
    error: unknown,
    base: string,
    source = '',
    line = 0,
    column = 0,
): FrontendDiagnostic {
    let name = '',
        message = '',
        stack = '';
    try {
        if (error instanceof Error) {
            name = error.name;
            message = error.message.slice(0, 8192);
            stack = error.stack?.slice(0, 16384) || '';
        } else if (typeof error === 'string') message = error.slice(0, 8192);
    } catch {
        /* A hostile getter must not break the error handler */
    }
    const frames: FrontendFrame[] = [];
    if (source)
        frames.push({
            component: component(source, base),
            line: number(line),
            column: number(column),
        });
    for (const row of stack.split('\n').slice(1, 25)) {
        const match = row.match(
            /((?:https?|tauri|asset|file):\/\/[^\s)]+|\/(?:src|assets|node_modules)\/[^\s)]+):(\d+):(\d+)/,
        );
        if (!match) continue;
        const frame = {
            component: component(match[1], base),
            line: number(Number(match[2])),
            column: number(Number(match[3])),
        };
        if (
            !frames.some(
                (previous) =>
                    previous.component === frame.component &&
                    previous.line === frame.line &&
                    previous.column === frame.column,
            )
        )
            frames.push(frame);
        if (frames.length === 12) break;
    }
    return {
        event,
        reason: event === 'resource_error' ? 'resource_load' : reason(message, name),
        frames,
    };
}

const resourceRoutes = [
    'editor',
    'action-library',
    'macros',
    'plugins',
    'settings',
    'diagnostics',
] as const;
const resourceOwners = [
    'sd-action-library',
    'sd-action-workspace',
    'sd-app-shell',
    'sd-device-output-dialog',
    'sd-device-preview',
    'sd-editor',
    'sd-inspector',
    'sd-macros',
    'sd-media-library',
    'sd-media-options',
    'sd-plugin-action-group',
    'sd-plugin-catalog-dialog',
    'sd-plugins',
    'sd-settings',
    'sd-video-editor',
] as const;
const resourceRoles = [
    'binding-icon',
    'control-surface',
    'background-preview',
    'media-preview',
    'thumbnail',
    'interface-icon',
    'plugin-icon',
] as const;

function recognized<T extends string>(values: readonly T[], input?: string | null): T | 'unknown' {
    return values.find((value) => value === input) ?? 'unknown';
}

function resourceContext(target: EventTarget | null): ResourceContext {
    const element = typeof Element !== 'undefined' && target instanceof Element ? target : null;
    return {
        route: recognized(
            resourceRoutes,
            globalThis.location?.hash ? globalThis.location.hash.slice(2) : 'editor',
        ),
        owner: recognized(
            resourceOwners,
            element
                ?.closest('[data-acl-component]')
                ?.getAttribute('data-acl-component')
                ?.toLowerCase(),
        ),
        role: recognized(resourceRoles, element?.getAttribute('data-resource-role')),
    };
}

type Send = (report: FrontendDiagnostic) => Promise<unknown>;
// Attach browser error reporting and return a cleanup handle
export function installFrontendDiagnostics(
    host: Pick<Window, 'addEventListener' | 'removeEventListener'>,
    output: Pick<Console, 'error'>,
    base: string,
    send: Send,
    now = Date.now,
) {
    let active = true,
        started = now(),
        count = 0,
        startupCount = 0;
    const recent = new Map<string, number>();
    // Forward a normalized diagnostic through the configured reporting callback
    const report = (
        event: FrontendEvent,
        error: unknown,
        source = '',
        line = 0,
        column = 0,
        resource?: ResourceContext,
        decoded = false,
        startup?: FrontendStartupContext,
    ) => {
        if (!active) return;
        const value = frontendDiagnostic(event, error, base, source, line, column);
        if (resource) value.resource = resource;
        if (startup) value.startup = startup;
        if (decoded) value.reason = 'resource_decode';
        const time = now();
        if (time - started >= 60_000) {
            started = time;
            count = 0;
            startupCount = 0;
            recent.clear();
        }
        const signature = JSON.stringify(value);
        if (
            (startup ? startupCount >= 3 : count >= 20) ||
            (recent.has(signature) && time - recent.get(signature)! < 5000)
        )
            return;
        recent.set(signature, time);
        if (startup) startupCount++;
        else count++;
        try {
            return send(value).then(
                () => {},
                () => {},
            );
        } catch {
            /* Logging never reports its own failures */
        }
    };
    // Report an uncaught browser error with its source context
    const onError = (event: Event) => {
        const error = event as ErrorEvent;
        if (error.error || error.message)
            report(
                'runtime_error',
                error.error || error.message,
                error.filename,
                error.lineno,
                error.colno,
            );
        else {
            report('resource_error', undefined, '', 0, 0, resourceContext(event.target));
        }
    };
    // Report an unhandled promise rejection
    const onRejection = (event: Event) =>
        report('unhandled_rejection', (event as PromiseRejectionEvent).reason);
    // Report a browser content-security-policy violation
    const onCsp = () => report('csp_violation', undefined);
    const original = output.error;
    // Preserve console output while reporting a normalized console error
    const wrapped: typeof output.error = (...args: unknown[]) => {
        original.apply(output, args);
        report(
            'console_error',
            args.find((value) => value instanceof Error) ||
                args.find((value) => typeof value === 'string'),
        );
    };
    output.error = wrapped;
    host.addEventListener('error', onError, true);
    host.addEventListener('unhandledrejection', onRejection);
    host.addEventListener('securitypolicyviolation', onCsp);
    // Remove the installed frontend diagnostic listeners
    const dispose = () => {
        active = false;
        host.removeEventListener('error', onError, true);
        host.removeEventListener('unhandledrejection', onRejection);
        host.removeEventListener('securitypolicyviolation', onCsp);
        host.removeEventListener('pagehide', dispose);
        if (output.error === wrapped) output.error = original;
    };
    host.addEventListener('pagehide', dispose, { once: true });
    return { report, dispose };
}

let reporter: ReturnType<typeof installFrontendDiagnostics> | undefined;
// Start frontend reporting with the Manager logging bridge
export function startFrontendDiagnostics() {
    if (reporter) return;
    reporter = installFrontendDiagnostics(window, console, location.href, async (report) => {
        if (import.meta.env.MODE !== 'public-preview' && isTauri())
            await invoke('report_frontend_error', { report });
        else
            window.dispatchEvent(new CustomEvent('manager-diagnostic-preview', { detail: report }));
    });
}
// Report a handled frontend failure with its component context
export function reportFrontendError(event: FrontendEvent, error: unknown) {
    reporter?.report(event, error);
}

// Keep startup phase and exact, finite ACL identifiers instead of logging error text
const startupLoaderCodes: readonly StartupLoaderCode[] = [
    'ACL_ASSET_LOAD_FAILED',
    'ACL_COMPONENT_LOAD_FAILED',
    'ACL_MANIFEST_FETCH_FAILED',
    'ACL_MANIFEST_HTTP_ERROR',
    'ACL_MANIFEST_INVALID',
    'ACL_MANIFEST_INVALID_JSON',
    'ACL_MANIFEST_TIMEOUT',
    'ACL_MANIFEST_TOO_LARGE',
    'ACL_TEMPLATE_FETCH_FAILED',
    'ACL_TEMPLATE_HTTP_ERROR',
    'ACL_TEMPLATE_TIMEOUT',
    'ACL_TEMPLATE_TOO_LARGE',
];

export function reportStartupFailure(
    stage: FrontendStartupContext['stage'],
    error: unknown,
    code?: unknown,
) {
    const loader_code =
        stage === 'shell_mount' ? startupLoaderCodes.find((value) => value === code) : undefined;
    const startup = loader_code ? { stage, loader_code } : { stage };
    return reporter?.report('startup_failed', error, '', 0, 0, undefined, false, startup);
}

export function reportDecodeFailure(
    owner: ResourceContext['owner'],
    role: ResourceContext['role'],
) {
    reporter?.report(
        'resource_error',
        undefined,
        '',
        0,
        0,
        {
            route: recognized(resourceRoutes, globalThis.location?.hash.slice(2)),
            owner,
            role,
        },
        true,
    );
}
