import type {
    ConfigurationRecoveryDocument,
    ConfigurationRecoveryVersion,
    ConfigurationRecoveryVersions,
} from '../generated/runtime';
import { diffJson } from './recovery-json-diff';
export interface RecoveryVersionsApi {
    list(id: string, revision: string): Promise<ConfigurationRecoveryVersions>;
    read(id: string, revision: string, version_id: string): Promise<ConfigurationRecoveryDocument>;
}
// Locate a recovery control by ID with its expected element type
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
export class RecoveryVersionViewer {
    private dialog = el<HTMLDialogElement>('recovery-versions');
    private select = el<HTMLSelectElement>('recovery-versions-select');
    private source: ConfigurationRecoveryDocument | null = null;
    private candidate: ConfigurationRecoveryDocument | null = null;
    private candidateValid = false;
    private versions: ConfigurationRecoveryVersion[] = [];
    private draft = '';
    private generation = 0;
    private busy = false;
    // Initialize recovery versions state and its required lifecycle hooks
    constructor(
        private api: RecoveryVersionsApi,
        private confirm: (title: string, message: string, action: string) => Promise<boolean>,
        private apply: (json: string) => void,
        private notify: (message: string) => void = () => {},
    ) {
        this.select.addEventListener('change', () => void this.choose());
        el('recovery-versions-load').addEventListener('click', () => void this.load());
        for (const id of ['recovery-versions-close', 'recovery-versions-cancel'])
            el(id).addEventListener('click', () => this.close());
        this.dialog.addEventListener('cancel', () => {
            this.generation++;
        });
    }
    // Derive open from the current recovery versions state
    get open() {
        return this.dialog.open;
    }
    // Return a snapshot of the current recovery versions state
    private state(busy: boolean, error = '') {
        this.busy = busy;
        this.select.disabled = busy;
        el<HTMLButtonElement>('recovery-versions-load').disabled =
            busy || !this.candidate || !this.candidateValid;
        if (error) this.notify(error);
    }
    // Release recovery versions state and associated lifecycle resources
    private close() {
        this.generation++;
        this.dialog.close();
    }
    // Show the selected recovery versions content
    async show(document: ConfigurationRecoveryDocument, draft: string, profile: boolean) {
        const generation = ++this.generation;
        this.source = document;
        this.candidate = document;
        this.candidateValid = document.issues.length === 0;
        this.draft = draft;
        this.versions = [];
        this.select.replaceChildren(new Option('Currently saved JSON', ''));
        this.paint();
        this.dialog.showModal();
        this.state(profile);
        el('recovery-versions-status').textContent = profile
            ? 'Checking retained history and local backups…'
            : 'Previous-version lookup is available for profiles. You can still compare this draft with its saved JSON.';
        if (!profile) return;
        try {
            const result = await this.api.list(document.id, document.revision);
            if (generation !== this.generation) return;
            if (result.revision !== document.revision)
                throw Error('Saved data changed. Close comparison and reload the saved version.');
            this.versions = result.versions;
            for (const version of result.versions)
                this.select.add(
                    new Option(
                        `${version.label} · ${version.source} · ${version.valid ? 'Valid profile' : 'Needs repair'}${version.timestamp_ms === null ? '' : ` · ${new Date(version.timestamp_ms).toLocaleString()}`}`,
                        version.id,
                    ),
                );
            el('recovery-versions-status').textContent = [
                result.versions.length
                    ? `${result.versions.length} previous version(s). Valid means current profile validation, not hardware or media availability.`
                    : 'No retained previous versions were found for this profile.',
                ...result.warnings,
            ].join(' ');
            this.state(false);
        } catch (error) {
            if (generation === this.generation) this.state(false, String(error));
        }
    }
    // Choose the active item for the recovery versions workflow
    private async choose() {
        if (!this.source || this.busy) return;
        const generation = ++this.generation,
            id = this.select.value;
        this.candidate = null;
        this.state(true);
        try {
            const candidate = id
                ? await this.api.read(this.source.id, this.source.revision, id)
                : this.source;
            if (generation !== this.generation) return;
            if (candidate.id !== this.source.id || candidate.revision !== this.source.revision)
                throw Error('Saved data changed. Reopen this comparison.');
            this.candidate = candidate;
            this.candidateValid =
                candidate.issues.length === 0 &&
                (!id || this.versions.find((version) => version.id === id)?.valid === true);
            this.paint();
            this.state(false);
        } catch (error) {
            if (generation === this.generation) {
                el('recovery-diff').replaceChildren();
                this.state(false, String(error));
            }
        }
    }
    // Paint the current recovery versions frame
    private paint() {
        if (!this.candidate) return;
        const result = diffJson(this.candidate.json, this.draft);
        const added = result.lines.filter((line) => line.kind === 'added').length,
            removed = result.lines.filter((line) => line.kind === 'removed').length;
        el('recovery-diff-summary').textContent = [
            added || removed
                ? `${result.truncated ? 'Preview: ' : ''}${removed} removed, ${added} added lines.`
                : this.candidate.json === this.draft
                  ? 'No changes.'
                  : 'Changes exceed the preview limit.',
            result.coarse ? 'Large changed blocks are shown as remove/add groups.' : '',
            result.truncated
                ? 'Preview limited to 2,000 lines / 500,000 characters. The full JSON is preserved when loaded.'
                : '',
            ...this.candidate.issues,
            !this.candidateValid ? 'This version needs repair and cannot be loaded.' : '',
        ]
            .filter(Boolean)
            .join(' ');
        const fragment = document.createDocumentFragment();
        for (const line of result.lines) {
            const row = document.createElement('div');
            row.className = `recovery-diff-line recovery-diff-${line.kind}`;
            for (const number of [line.before, line.after]) {
                const span = document.createElement('span');
                span.className = 'recovery-diff-number';
                span.textContent = number === null ? '' : String(number);
                row.append(span);
            }
            const sign = document.createElement('span');
            sign.className = 'recovery-diff-sign';
            sign.textContent = line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' ';
            row.append(sign);
            const text = document.createElement('code');
            text.textContent = line.text;
            row.append(text);
            fragment.append(row);
        }
        el('recovery-diff').replaceChildren(fragment);
    }
    // Load the data required by the recovery versions workflow
    private async load() {
        if (this.busy || !this.candidate || !this.candidateValid) return;
        const candidate = this.candidate,
            generation = this.generation;
        const accepted = await this.confirm(
            'Load selected JSON into editor?',
            'This replaces the current editor text, including unsaved edits. Nothing is saved yet. Validate and Save applies only this item’s JSON, not plugins, media files, shared macros, or other settings.',
            'Load into editor',
        );
        if (!accepted || generation !== this.generation) return;
        this.apply(candidate.json);
        this.close();
    }
}
