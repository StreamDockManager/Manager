import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { PermissionScope } from '../generated/runtime';
import { permissionLabel, scopeDetails } from './permission-labels';
import { applyTheme, readAppearance } from './theme-style';
import './styles.css';

const systemTheme = matchMedia('(prefers-color-scheme: dark)');
function refreshAppearance() {
    const { preference, style } = readAppearance();
    applyTheme(preference, systemTheme.matches, style);
}
refreshAppearance();
systemTheme.addEventListener('change', refreshAppearance);
window.addEventListener('storage', refreshAppearance);

const id = new URLSearchParams(location.search).get('id');
const element = (key: string) => document.getElementById(key)!;
const input = element('text') as HTMLTextAreaElement;
const confirm = element('confirm') as HTMLButtonElement;
let choice = '';
type View = {
    kind: 'permissions' | 'transfer' | 'sending' | 'result' | 'failed' | 'select' | 'webhook';
    page?: { entries: { id: string; name: string }[]; next: number | null };
    start?: number;
    scope?: PermissionScope;
    authorization?: boolean;
    plugin: string;
    action: string;
    scopes?: PermissionScope[];
    text?: string;
    origin?: string;
    purpose?: string;
    copy?: boolean;
};

async function show() {
    const v = await invoke<View>('permission_window_view', { id });
    element('identity').textContent = v.plugin + ' · ' + v.action;
    const permission = v.kind === 'permissions',
        transfer = v.kind === 'transfer',
        result = v.kind === 'result';
    const picker = v.kind === 'select';
    const webhook = v.kind === 'webhook';
    const entries = element('entry') as HTMLSelectElement;
    element('picker').hidden = !picker;
    entries.replaceChildren(new Option('Choose an entry', ''));
    for (const item of v.page?.entries ?? []) entries.add(new Option(item.name, item.id));
    element('previous').hidden = !v.start;
    element('next').hidden = v.page?.next == null;
    element('title').textContent = webhook
        ? 'Review local webhook'
        : picker
          ? 'Choose a Vault entry'
          : v.kind === 'failed'
            ? 'Request failed'
            : permission
              ? 'Review plugin permissions'
              : transfer
                ? 'Review clipboard transfer'
                : result
                  ? 'Action result'
                  : 'Sending';
    element('details').textContent = webhook
        ? 'Send the example ping to ' + v.origin + '/webhooks/manager?'
        : picker && v.scope
          ? permissionLabel(v.scope) + ' · ' + scopeDetails(v.scope) + ' · ' + v.purpose
          : v.kind === 'failed'
            ? 'The provider request failed. Press the dock control to start a new request.'
            : permission
              ? 'Purpose stated by the plugin: ' + v.purpose
              : transfer
                ? 'Send the text below to Example Summarizer at ' +
                  v.origin +
                  (v.authorization ? ' using the selected Vault credential.' : '')
                : '';
    const scopes = element('scopes');
    scopes.replaceChildren();
    for (const scope of v.scopes ?? []) {
        const item = document.createElement('li');
        item.textContent = permissionLabel(scope) + ' · ' + scopeDetails(scope);
        scopes.append(item);
    }
    scopes.hidden = !permission;
    element('field').hidden = !(transfer || result);
    element('label').textContent = transfer ? 'Text to send' : 'Result';
    input.readOnly = !transfer;
    input.value = v.text ?? '';
    element('status').textContent =
        result && !v.copy ? 'Copy is unavailable. You can select the result text.' : '';
    choice = webhook
        ? 'webhook'
        : picker
          ? 'select'
          : permission
            ? 'allow'
            : transfer
              ? 'send'
              : 'copy';
    confirm.textContent = webhook
        ? 'Send ping'
        : picker
          ? 'Approve this entry'
          : permission
            ? 'Allow requested access'
            : transfer
              ? 'Send'
              : 'Copy';
    confirm.hidden = !(permission || transfer || picker || webhook || (result && v.copy));
    confirm.disabled = picker;
    element('close').textContent = permission ? 'Deny' : transfer ? 'Cancel' : 'Close';
    element('close').focus();
}
element('entry').addEventListener('change', () => {
    confirm.disabled = !(element('entry') as HTMLSelectElement).value;
});
for (const key of ['previous', 'next'])
    element(key).addEventListener('click', async () => {
        try {
            await invoke('permission_window_reply', { id, choice: key, text: null });
            await show();
        } catch {
            await getCurrentWindow().close();
        }
    });
element('close').addEventListener('click', async () => {
    try {
        await invoke('permission_window_reply', { id, choice: 'deny', text: null });
    } finally {
        await getCurrentWindow().close();
    }
});
window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') void getCurrentWindow().close();
});
confirm.addEventListener('click', async () => {
    confirm.disabled = true;
    element('error').textContent = '';
    try {
        const text =
            choice === 'send'
                ? input.value
                : choice === 'select'
                  ? (element('entry') as HTMLSelectElement).value
                  : null;
        if (text !== null && new TextEncoder().encode(text).length > 65536)
            throw new Error('limit');
        if (choice === 'send') element('status').textContent = 'Sending reviewed text…';
        await invoke('permission_window_reply', {
            id,
            choice: choice === 'webhook' ? 'send' : choice,
            text,
        });
        input.value = '';
        if (choice === 'send' || choice === 'webhook') await show();
    } catch {
        element('error').textContent =
            choice === 'copy'
                ? 'Manager could not copy the result. You can select the text or try Copy again.'
                : 'The request could not be completed. Check the text limit and current permissions.';
        element('status').textContent = '';
        confirm.disabled = false;
    }
});
void show().catch(() => getCurrentWindow().close());
