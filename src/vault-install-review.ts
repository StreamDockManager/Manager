import type {
    VaultEntry,
    VaultKind,
    VaultEntryInput,
    VaultStatus,
    VaultSetupSlot as NativeVaultSetupSlot,
} from '../generated/runtime';

export type VaultSetupSlot = Omit<NativeVaultSetupSlot, 'entry_kind' | 'kind' | 'mode'> & {
    entry_kind: VaultKind;
    kind: 'secret' | 'value' | 'location';
    mode: 'broker' | 'direct';
    label?: string;
    origin?: string | null;
};

type SetupSelection = { action: string; slot_id: string; entry: VaultEntry };
export interface VaultSetupService {
    list(
        id: string,
        slotId: string,
        action: string,
    ): Promise<{ ticket: string; entries: VaultEntry[] }>;
    cancel(id: string, ticket: string): Promise<void>;
    create?(
        id: string,
        slotId: string,
        action: string,
        ticket: string,
        input: VaultEntryInput,
    ): Promise<VaultEntry>;
    status?(): Promise<VaultStatus>;
    unlock?(): Promise<VaultStatus>;
    initialize?(): Promise<VaultStatus>;
    permission(id: string, slotId: string, action: string, allow: boolean): Promise<void>;
    select(
        id: string,
        slotId: string,
        action: string,
        ticket: string,
        entryId: string,
        revision: string,
        allowDirectRead: boolean,
    ): Promise<SetupSelection>;
}

export class VaultInstallReview {
    private generation = 0;
    private ticket = '';
    private reviewId = '';
    private optIns = new Set<string>();
    private selected: SetupSelection[] = [];
    slot: VaultSetupSlot | null = null;
    entries: VaultEntry[] = [];
    pending: VaultEntry | null = null;
    mode: VaultStatus['state'] | null = null;
    unlockMode: string | null = null;
    creating = false;
    opener: HTMLElement | null = null;
    working = false;
    error = '';

    constructor(private service: VaultSetupService) {}

    selection(review: string, slotId: string, action: string) {
        return this.reviewId === review
            ? this.selected.find(
                  (selection) => selection.slot_id === slotId && selection.action === action,
              )
            : undefined;
    }

    ready(review: string, slots: VaultSetupSlot[]) {
        return slots.every(
            (slot) => !slot.required || !!this.selection(review, slot.id, slot.action),
        );
    }

    close() {
        ++this.generation;
        if (this.ticket) void this.service.cancel(this.reviewId, this.ticket).catch(() => {});
        this.ticket = '';
        this.slot = null;
        this.entries = [];
        this.pending = null;
        this.mode = null;
        this.unlockMode = null;
        this.creating = false;
        this.working = false;
        this.error = '';
    }

    reset() {
        this.close();
        this.reviewId = '';
        this.opener = null;
        this.selected = [];
        this.optIns.clear();
    }

    async open(review: string, slot: VaultSetupSlot, opener?: HTMLElement) {
        if (this.reviewId !== review) {
            this.reset();
            this.reviewId = review;
        } else this.close();
        const generation = this.generation;
        this.slot = slot;
        this.opener = opener ?? this.opener;
        this.working = true;
        try {
            const status = await this.service.status?.();
            if (this.generation !== generation) return;
            this.mode = status?.state ?? null;
            this.unlockMode = status?.mode ?? null;
            if (status && status.state !== 'unlocked') return;
            const choices = await this.service.list(review, slot.id, slot.action);
            if (this.generation === generation) {
                this.entries = choices.entries;
                this.ticket = choices.ticket;
            } else await this.service.cancel(review, choices.ticket);
        } catch {
            if (this.generation === generation)
                this.error = 'Vault entries are unavailable. Unlock Vault and try again.';
        } finally {
            if (this.generation === generation) this.working = false;
        }
    }

    get canShowCreate() {
        return (
            !!this.service.create &&
            !!this.slot &&
            ['api-key', 'bearer-token', 'credential', 'text', 'location'].includes(
                this.slot.entry_kind,
            ) &&
            (this.mode === 'unlocked' || this.mode == null)
        );
    }

    get canCreate() {
        return this.canShowCreate && !this.working;
    }

    get pickerStatus() {
        if (this.error) return '';
        if (this.working && this.mode && this.mode !== 'unlocked') return 'Opening Vault…';
        if (this.working)
            return this.creating
                ? 'Creating entry…'
                : this.ticket
                  ? 'Updating Vault access…'
                  : 'Loading Vault entries…';
        if (this.mode && this.mode !== 'unlocked' && this.mode !== 'uninitialized')
            return 'Vault must be unlocked before entries can be selected.';
        return !this.creating && !this.entries.length && (!this.mode || this.mode === 'unlocked')
            ? 'No compatible entries.'
            : '';
    }

    get locationSlot() {
        return this.slot?.kind === 'location';
    }

    get creatableSecret() {
        return this.slot?.kind === 'secret';
    }

    async makeVaultAvailable(initialize: boolean) {
        const operation = initialize ? this.service.initialize : this.service.unlock;
        const slot = this.slot;
        if (
            !operation ||
            !slot ||
            this.working ||
            this.mode !== (initialize ? 'uninitialized' : 'locked') ||
            (!initialize && this.unlockMode !== 'os' && this.unlockMode !== 'password')
        )
            return;
        const generation = this.generation;
        this.working = true;
        this.error = '';
        try {
            const status = await operation();
            if (this.generation !== generation) return;
            this.mode = status.state;
            if (status.state === 'unlocked') await this.open(this.reviewId, slot);
        } catch {
            if (this.generation === generation) this.error = 'Vault could not be opened.';
        } finally {
            if (this.generation === generation) this.working = false;
        }
    }

    async createEntry(event: SubmitEvent) {
        const slot = this.slot;
        if (!slot || !this.canCreate || !this.creating) return;
        const form = event.currentTarget as HTMLFormElement;
        if (!form.reportValidity()) return;
        const data = new FormData(form);
        const label = String(data.get('name') ?? '').trim();
        const latitude = form.elements.namedItem('latitude') as HTMLInputElement | null;
        const longitude = form.elements.namedItem('longitude') as HTMLInputElement | null;
        const value = this.locationSlot
            ? {
                  name: label,
                  latitude: latitude?.valueAsNumber ?? NaN,
                  longitude: longitude?.valueAsNumber ?? NaN,
              }
            : String(data.get('value') ?? '');
        if (
            typeof value === 'object' &&
            !(
                Number.isFinite(value.latitude) &&
                Math.abs(value.latitude) <= 90 &&
                Number.isFinite(value.longitude) &&
                Math.abs(value.longitude) <= 180
            )
        ) {
            this.error = 'Enter valid latitude and longitude.';
            return;
        }
        const input = form.querySelector<HTMLInputElement>('[name="value"]');
        if (input) input.value = '';
        if (!label || !value) {
            this.error = 'Enter a name and value.';
            return;
        }
        const generation = this.generation;
        this.working = true;
        this.error = '';
        try {
            const entry = await this.service.create!(
                this.reviewId,
                slot.id,
                slot.action,
                this.ticket,
                {
                    label,
                    kind: slot.entry_kind,
                    protected: data.has('protected'),
                    value,
                },
            );
            if (this.generation !== generation) return;
            this.entries = [...this.entries, entry];
            this.creating = false;
            this.pending = entry;
        } catch {
            if (this.generation === generation)
                this.error =
                    'The entry could not be created. Check the current Vault and try again.';
        } finally {
            if (this.generation === generation) this.working = false;
        }
    }

    cancelPicker() {
        this.close();
        this.opener?.focus({ preventScroll: true });
        this.opener = null;
    }

    choose(id: string) {
        if (!this.slot || this.working) return;
        this.pending = this.entries.find((entry) => entry.id === id) ?? null;
    }

    get directRead() {
        return (
            (this.slot?.kind === 'secret' || this.slot?.kind === 'location') &&
            this.slot.mode === 'direct'
        );
    }

    get packageApproved() {
        return !!this.slot && this.optIns.has(this.slot.action + '/' + this.slot.id);
    }

    async setPackageApproval(allow: boolean) {
        const slot = this.slot;
        if (!slot || !this.directRead || this.working) return;
        const generation = this.generation;
        const key = slot.action + '/' + slot.id;
        this.working = true;
        this.error = '';
        try {
            await this.service.permission(this.reviewId, slot.id, slot.action, allow);
            if (this.generation !== generation) return;
            if (allow) this.optIns.add(key);
            else {
                this.optIns.delete(key);
                this.selected = this.selected.filter(
                    (item) => item.action !== slot.action || item.slot_id !== slot.id,
                );
            }
        } catch {
            if (this.generation === generation)
                this.error = 'Package permission could not be changed. Unlock Vault and try again.';
        } finally {
            if (this.generation === generation) this.working = false;
        }
    }

    async confirm(review: string, allowDirectRead: boolean) {
        const slot = this.slot;
        const entry = this.pending;
        if (
            this.reviewId !== review ||
            !slot ||
            !entry ||
            this.working ||
            (this.directRead && (!allowDirectRead || !this.packageApproved))
        )
            return;
        const generation = this.generation;
        this.working = true;
        this.error = '';
        try {
            const selected = await this.service.select(
                review,
                slot.id,
                slot.action,
                this.ticket,
                entry.id,
                entry.revision,
                this.directRead && allowDirectRead,
            );
            if (this.generation !== generation) return;
            this.ticket = '';
            if (
                selected.action !== slot.action ||
                selected.slot_id !== slot.id ||
                selected.entry.id !== entry.id
            )
                throw Error('The selected entry changed.');
            this.selected = [
                ...this.selected.filter(
                    (item) => item.slot_id !== slot.id || item.action !== slot.action,
                ),
                selected,
            ];
            this.cancelPicker();
        } catch {
            if (this.generation === generation)
                this.error = 'This entry could not be approved. Close the picker and try again.';
        } finally {
            if (this.generation === generation) this.working = false;
        }
    }
}
