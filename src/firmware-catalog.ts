import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type {
    FirmwareCatalogReport,
    FirmwareCandidate,
    FirmwareStatus,
} from '../generated/runtime';
import { ownWindowMethods } from './window-work';
import { nextTab, tabNavigationKey } from './tab-navigation';
let firmwarePanelSequence = 0;

export function firmwareTime(seconds: number | null): string {
    return seconds === null
        ? 'Never checked'
        : new Date(seconds * 1000).toLocaleString(undefined, { timeZoneName: 'short' });
}

export function createFirmwareCatalog(app: () => AppState) {
    const panelPrefix = `firmware-${++firmwarePanelSequence}`;
    return {
        activeTab: 'standard',
        tabs: [
            { id: 'standard', label: 'Updates' },
            { id: 'test', label: 'Test firmware' },
            { id: 'sources', label: 'Sources' },
        ],
        tabId(id: string) {
            return `${panelPrefix}-tab-${id}`;
        },
        panelId(id: string) {
            return `${panelPrefix}-panel-${id}`;
        },
        selectTab(id: string) {
            if (this.tabs.some((tab) => tab.id === id)) this.activeTab = id;
        },
        navigateTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            event.stopPropagation();
            const next = nextTab(
                this.tabs.map((tab) => tab.id),
                this.activeTab,
                event.key,
            );
            if (!next) return;
            this.selectTab(next);
            this.element?.querySelector<HTMLElement>(`#${this.tabId(next)}`)?.focus();
        },
        sourceState(state: string) {
            return (
                (
                    {
                        current: 'Current',
                        partial: 'Partial results',
                        stale: 'Cached',
                        unchecked: 'Not checked',
                    } as Record<string, string>
                )[state] || state
            );
        },
        report: null as FirmwareCatalogReport | null,
        installed: null as FirmwareStatus | null,
        busy: false,
        error: '',
        instructionsBusy: false,
        instructionsError: '',
        sequence: 0,
        element: null as HTMLElement | null,
        get app() {
            return app();
        },
        get identity() {
            const device = app().device;
            return JSON.stringify([
                device?.id,
                device?.connected,
                device?.model_id,
                device?.variant_id,
            ]);
        },
        get standardCandidates() {
            return (
                this.report?.candidates.filter((c) => c.source !== 'test' && !c.equivalent_to) || []
            );
        },
        get testCandidates() {
            return this.report?.include_test
                ? this.report.candidates.filter((c) => c.source === 'test' && !c.equivalent_to)
                : [];
        },
        get candidateGroups() {
            return [
                {
                    id: 'standard',
                    title: 'Standard catalogs',
                    visible: this.activeTab === 'standard',
                    candidates: this.standardCandidates,
                },
                {
                    id: 'test',
                    title: 'Test firmware',
                    visible: this.activeTab === 'test',
                    candidates: this.testCandidates,
                },
            ];
        },
        get installedLabel() {
            const installed = this.report?.installed || this.installed?.installed;
            return installed
                ? installed +
                      (this.report?.installed_stale || this.installed?.stale ? ' (last known)' : '')
                : 'Unknown';
        },
        get resultLabel() {
            if (this.error && this.report) return 'Check incomplete';
            const labels: Record<string, string> = {
                update_available: 'Update available',
                matches: 'Installed firmware matches catalog',
                installed_newer: 'Installed firmware is newer',
                no_match: 'No matching firmware listed',
                compatibility_unknown: 'Compatibility needs confirmation',
                version_unknown: 'Version cannot be compared',
                incomplete: 'Check incomplete',
                discovery: 'Compatibility needs confirmation',
                not_checked: 'Not checked yet',
                unsupported: 'Discovery unavailable',
            };
            return (
                labels[this.report?.state || 'not_checked'] ||
                this.report?.message ||
                'Not checked yet'
            );
        },
        get lastChecked() {
            const times = (this.report?.sources || [])
                .map((source) => source.last_checked)
                .filter((time): time is number => time !== null);
            return firmwareTime(times.length ? Math.max(...times) : null);
        },
        sourceTime: firmwareTime,
        sourceName(source: string) {
            return (
                {
                    mirabox: 'Mirabox',
                    vsd_craft: 'VSD Craft',
                    test: 'Extended test catalog',
                    generic: 'Generic StreamDock',
                }[source] || source
            );
        },
        reasonText(reason: string) {
            const labels: Record<string, string> = {
                artifact_urls_disagree: 'The catalog lists conflicting artifact URLs.',
                version_missing: 'The catalog version is missing.',
                checksum_malformed: 'The vendor checksum is malformed.',
                unsupported_constraints: 'Vendor compatibility constraints are not yet understood.',
                unknown_record_fields: 'The record contains unsupported metadata.',
                version_filename_conflict: 'Version and filename labels conflict.',
                unresolved_device_variant: 'The device variant cannot be confirmed.',
                target_version_conflict: 'Sources disagree on the version at this URL.',
                target_checksum_conflict: 'Sources disagree on the checksum at this URL.',
                version_target_conflict: 'The same version refers to different artifacts.',
            };
            return labels[reason] || reason.replaceAll('_', ' ');
        },
        candidateName(candidate: FirmwareCandidate) {
            return [
                candidate.model_key,
                candidate.variant_key,
                candidate.version || 'Version unavailable',
            ]
                .filter(Boolean)
                .join(' · ');
        },
        candidateSummary(candidate: FirmwareCandidate) {
            if (candidate.matching === 'ambiguous') return 'Compatibility needs confirmation';
            if (candidate.matching === 'unmatched') return 'Not a confirmed device match';
            if (candidate.reasons.length || candidate.comparison === 'unavailable')
                return 'Version cannot be compared';
            return (
                {
                    newer: 'Newer catalog version',
                    equal: 'Matches installed version',
                    older: 'Installed firmware is newer',
                }[candidate.comparison] || 'Discovery only'
            );
        },
        rawDetails(candidate: FirmwareCandidate) {
            const references = (this.report?.candidates || []).filter(
                (row) => row.equivalent_to === candidate.id,
            );
            return JSON.stringify(
                {
                    record: candidate.raw,
                    equivalent_sources: references.map((row) => ({
                        source: row.source,
                        source_url: row.source_url,
                        snapshot_id: row.snapshot_id,
                        model_key: row.model_key,
                        variant_key: row.variant_key,
                        record: row.raw,
                    })),
                },
                null,
                2,
            );
        },
        invalidate() {
            this.activeTab = 'standard';
            this.instructionsBusy = false;
            this.instructionsError = '';
            this.sequence++;
            this.busy = false;
            this.report = null;
            this.installed = null;
            this.error = '';
        },
        async load(refresh = false) {
            const deviceId = app().deviceId;
            if (!deviceId || this.busy) return;
            const sequence = ++this.sequence;
            const identity = this.identity;
            this.busy = true;
            this.error = '';
            try {
                // Query installed firmware through the existing connection before evaluating the catalog.
                const installed = await app().command(
                    refresh ? 'check_firmware' : 'firmware_status',
                    { deviceId },
                );
                if (sequence !== this.sequence || identity !== this.identity) return;
                this.installed = installed;
                this.element?.dispatchEvent(
                    new CustomEvent('firmware-installed', { bubbles: true, detail: installed }),
                );
                const report = await app().command(
                    refresh ? 'refresh_firmware_catalog' : 'firmware_catalog_status',
                    { deviceId },
                );
                if (
                    sequence === this.sequence &&
                    identity === this.identity &&
                    report.device_id === deviceId
                )
                    this.report = report;
            } catch (error) {
                if (sequence === this.sequence && identity === this.identity)
                    this.error = 'Check incomplete: ' + String(error);
            } finally {
                if (sequence === this.sequence) this.busy = false;
            }
        },
        get instructionsAvailable() {
            return !!this.installed?.instructions && !!app().device?.connected;
        },
        async openInstructions() {
            if (!this.instructionsAvailable || this.instructionsBusy || this.busy) return;
            const sequence = this.sequence;
            const deviceId = app().deviceId;
            this.instructionsBusy = true;
            this.instructionsError = '';
            try {
                // Native code resolves the selected hardware configuration; never pass a catalog URL.
                await app().command('open_firmware_instructions', { deviceId });
            } catch (error) {
                if (sequence === this.sequence) this.instructionsError = String(error);
            } finally {
                if (sequence === this.sequence) this.instructionsBusy = false;
            }
        },
        async check() {
            await this.load(true);
        },
        async toggleTest(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const enabled = input.checked;
            if (this.busy) return;
            const sequence = ++this.sequence;
            this.busy = true;
            this.error = '';
            try {
                await app().command('set_firmware_catalog_test', { enabled });
                if (sequence !== this.sequence) return;
                // Hide disabled test results immediately; enabling alone does not fetch the test feed.
                if (this.report) this.report.include_test = enabled;
            } catch (error) {
                if (sequence === this.sequence) this.error = String(error);
            } finally {
                if (sequence === this.sequence) {
                    this.busy = false;
                    input.checked = !!this.report?.include_test;
                }
            }
        },
        init() {
            this.element = (this as unknown as { $el: HTMLElement }).$el;
            ownWindowMethods(this, 'firmware catalog', [
                'load',
                'check',
                'toggleTest',
                'openInstructions',
            ]);
            (this as unknown as { $watch: (key: string, callback: () => void) => void }).$watch(
                'identity',
                () => this.invalidate(),
            );
        },
        destroy() {
            this.invalidate();
        },
    };
}

export function registerFirmwareCatalog(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('firmwareCatalog', () => createFirmwareCatalog(app));
}
