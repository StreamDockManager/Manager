import type AlpineType from '@alpinejs/csp';
import type { Bridge } from './bridge';
import type { LocationMatch } from '../generated/runtime';

type Choice = LocationMatch & {
    source: 'open-meteo' | 'nominatim' | 'device';
    accuracy_m?: number;
    timestamp?: number;
};
type Session = {
    owner: HTMLDialogElement;
    opener: HTMLElement;
    token: string | null;
    vaultId: string | null;
    choose: (place: Choice) => void;
};

export function registerLocationPicker(Alpine: typeof AlpineType, bridge: Bridge) {
    let current: Session | null = null;
    const dialog = () => document.querySelector<HTMLDialogElement>('#location-picker')!;
    Alpine.data('locationPicker', () => ({
        endpoint: '',
        saved: '',
        providerReady: false,
        query: '',
        country: '',
        postal: false,
        busy: false,
        message: '',
        results: [] as Choice[],
        async open(event: CustomEvent<Session>) {
            if (current || !event.detail.owner.open) return;
            const session = event.detail;
            current = session;
            session.owner.addEventListener('close', this.cancelOwner, { once: true });
            this.endpoint = this.saved = this.query = this.country = this.message = '';
            this.results = [];
            this.postal = false;
            this.providerReady = false;
            this.busy = true;
            dialog().showModal();
            try {
                const endpoint = await bridge.command('location_provider');
                if (current === session) {
                    this.saved = this.endpoint = endpoint;
                    this.providerReady = true;
                }
            } catch {
                if (current === session)
                    this.message = 'Location services require the desktop app.';
            } finally {
                if (current === session) this.busy = false;
            }
        },
        cancelOwner() {
            dialog().close();
        },
        close() {
            dialog().close();
        },
        closed() {
            const session = current;
            if (!session) return;
            current = null;
            session.owner.removeEventListener('close', this.cancelOwner);
            if (this.busy) {
                const cancel = session.token
                    ? bridge.command('cancel_settings_location', { token: session.token })
                    : bridge.command('vault_cancel_location', { id: session.vaultId! });
                void cancel.catch(() => {});
            }
            this.results = [];
            this.query = this.country = this.message = '';
            this.busy = false;
            if (session.owner.open && session.opener.isConnected) session.opener.focus();
        },
        async saveProvider() {
            const session = current;
            if (!session || this.busy) return;
            this.busy = true;
            this.message = '';
            this.results = [];
            try {
                await bridge.command('set_location_provider', { endpoint: this.endpoint.trim() });
                const saved = await bridge.command('location_provider');
                if (current === session) {
                    this.saved = this.endpoint = saved;
                    this.message = 'Search provider saved.';
                }
            } catch (error) {
                if (current === session) this.message = String(error);
            } finally {
                if (current === session) this.busy = false;
            }
        },
        clearResults() {
            this.results = [];
            this.message = '';
        },
        async search() {
            const session = current;
            if (
                !session ||
                this.busy ||
                !this.providerReady ||
                this.saved !== this.endpoint ||
                !this.query.trim()
            )
                return;
            this.clearResults();
            this.busy = true;
            try {
                const rows = await bridge.command('search_locations', {
                    vaultId: session.vaultId,
                    token: session.token,
                    endpoint: this.saved,
                    query: this.query,
                    country: this.country,
                    postal: this.postal,
                });
                if (current !== session || !session.owner.open) return;
                const source = this.saved ? 'nominatim' : 'open-meteo';
                this.results = rows.map((row) => ({ ...row, source }));
                this.message = rows.length
                    ? 'Select a result to use its coordinates.'
                    : this.postal
                      ? 'No matching postal codes. Try searching by city instead.'
                      : 'No matching locations.';
            } catch (error) {
                if (current === session) this.message = String(error);
            } finally {
                if (current === session) this.busy = false;
            }
        },
        async gps() {
            const session = current;
            if (!session || this.busy) return;
            this.clearResults();
            this.busy = true;
            try {
                const fix = session.token
                    ? await bridge.command('request_settings_location', { token: session.token })
                    : await bridge.command('vault_current_location', { id: session.vaultId! });
                if (current !== session || !session.owner.open) return;
                this.results = [{ ...fix, source: 'device', label: 'Current location' }];
                this.message = 'Estimated accuracy ' + Math.round(fix.accuracy_m) + ' m.';
            } catch (error) {
                if (current === session) this.message = String(error);
            } finally {
                if (current === session) this.busy = false;
            }
        },
        choose(event: Event) {
            const row = this.results[Number((event.currentTarget as HTMLElement).dataset.index)];
            if (row && current?.owner.open && current.opener.isConnected) current.choose(row);
            this.close();
        },
    }));
    Alpine.data('locationFieldPicker', () => ({
        pick(event: Event) {
            const opener = event.currentTarget as HTMLButtonElement;
            const owner = opener.closest('dialog');
            if (!owner) return;
            const panel = Alpine.store('pluginSettings') as {
                token: string;
                open: boolean;
                location: boolean;
                sendLocation: (place: Choice) => void;
            };
            const settings = owner.id === 'plugin-settings';
            const token = settings ? panel.token : null;
            if (settings && (!panel.open || !panel.location || !token)) return;
            opener.focus();
            window.dispatchEvent(
                new CustomEvent<Session>('open-location-picker', {
                    detail: {
                        owner,
                        opener,
                        token,
                        vaultId: settings ? null : crypto.randomUUID(),
                        choose(place) {
                            if (settings) {
                                if (panel.open && panel.location && panel.token === token)
                                    panel.sendLocation(place);
                                return;
                            }
                            const form = opener.form;
                            if (!form || !opener.isConnected) return;
                            for (const [key, value] of [
                                ['latitude', place.latitude],
                                ['longitude', place.longitude],
                            ] as const) {
                                const input = form.elements.namedItem(key) as HTMLInputElement;
                                input.value = String(value);
                                input.dispatchEvent(new Event('input', { bubbles: true }));
                            }
                            if (place.source === 'device' && place.accuracy_m !== undefined) {
                                opener.dispatchEvent(
                                    new CustomEvent('vault-location-fix', {
                                        bubbles: true,
                                        detail: {
                                            latitude: place.latitude,
                                            longitude: place.longitude,
                                            accuracy_m: place.accuracy_m,
                                            timestamp: place.timestamp,
                                        },
                                    }),
                                );
                            }
                            const name = form.querySelector<HTMLInputElement>(
                                '[name="locationName"],[name="name"]',
                            );
                            if (name && (!name.value || name.name === 'locationName'))
                                name.value = Array.from(place.label).slice(0, 128).join('');
                        },
                    },
                }),
            );
        },
    }));
}
