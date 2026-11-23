import type AlpineType from '@alpinejs/csp';
import type { Manifest } from './types';
import type { AppState } from './state';
import type { Pattern } from '../generated/runtime';
import { positionedControlRect, previewGeometry } from './preview-layout';
import { native } from './bridge';

export function hasTestSurface(model?: Manifest) {
    return Boolean(
        model?.capabilities.background || model?.controls.some((c) => c.image_slot != null),
    );
}

export function registerDisplayTest(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('displayTest', () => {
        let token = '',
            generation = 0,
            revision = 0,
            device = '',
            until = 0;
        let timer: ReturnType<typeof setTimeout>;
        return {
            pattern: 'red' as Pattern,
            separateBackground: false,
            backgroundColor: '#404040',
            get hasBackground() {
                return Boolean(app().device?.manifest.capabilities.background);
            },
            get testBackground() {
                return this.hasBackground && this.separateBackground ? this.backgroundColor : null;
            },
            frame: 0,
            paused: false,
            running: false,
            message: '',
            get available() {
                const d = app().device;
                return !!(
                    d?.connected &&
                    !app().effectiveDeviceOutput.sleeping &&
                    hasTestSurface(d.manifest)
                );
            },
            get hasTouch() {
                return app().device?.manifest.controls.some(
                    (c) => c.kind === 'touch' && c.image_slot != null,
                );
            },
            deviceName: '',
            draw() {
                const model = app().device?.manifest;
                if (!model) return;
                const canvas = this.$refs.pattern as HTMLCanvasElement;
                const scale = Math.min(2, 960 / model.layout.width, 540 / model.layout.height);
                canvas.width = Math.ceil(model.layout.width * scale);
                canvas.height = Math.ceil(model.layout.height * scale);
                const ctx = canvas.getContext('2d')!;
                ctx.scale(scale, scale);
                const controls = model.controls.filter((c) => c.image_slot != null);
                const touch = controls.filter((c) => c.kind === 'touch');
                const geometry = previewGeometry(
                    model.controls,
                    model.layout.width,
                    model.model_id,
                    model.capabilities.touch_panel,
                );
                const solids = ['red', 'green', 'blue', 'white', 'black'];
                const color =
                    this.pattern === 'green'
                        ? '#00ff00'
                        : solids.includes(this.pattern)
                          ? this.pattern
                          : '#000';
                ctx.fillStyle = model.capabilities.background
                    ? this.testBackground || color
                    : '#292929';
                ctx.fillRect(0, 0, model.layout.width, model.layout.height);
                controls.forEach((c, i) => {
                    const r = positionedControlRect(geometry, c);
                    ctx.fillStyle = color;
                    ctx.fillRect(r.x, r.y, r.width, r.height);
                    ctx.strokeStyle = '#999';
                    ctx.strokeRect(r.x, r.y, r.width, r.height);
                    ctx.fillStyle = '#fff';
                    if (this.pattern === 'positions') {
                        ctx.font = Math.max(10, Math.min(32, r.height / 2)) + 'px sans-serif';
                        ctx.textAlign = 'center';
                        ctx.textBaseline = 'middle';
                        ctx.fillText(String(i + 1), r.x + r.width / 2, r.y + r.height / 2);
                    } else if (this.pattern === 'alignment') {
                        ctx.fillRect(r.x + r.width / 2, r.y, 1, r.height);
                        ctx.fillRect(r.x, r.y + r.height / 2, r.width, 1);
                    } else if (
                        this.pattern === 'marker' &&
                        touch[Math.floor(this.frame / 8) % touch.length]?.id === c.id
                    ) {
                        ctx.fillRect(
                            r.x + ((r.width - 3) * (this.frame % 8)) / 7,
                            r.y,
                            3,
                            r.height,
                        );
                    }
                });
            },
            async tick(id: number) {
                if (id !== generation || !this.running) return;
                if (
                    Date.now() >= until ||
                    device !== app().deviceId ||
                    !this.available ||
                    revision !== app().snapshot.configuration_revision
                ) {
                    await this.stop('Display test ended.');
                    return;
                }
                const sent = this.pattern,
                    frame = this.frame;
                try {
                    if (native) {
                        const { startDockPreview } = await import('./dock-preview');
                        if (id !== generation) return;
                        const next = await startDockPreview(
                            {
                                bindings: {},
                                background: null,
                                display_test: {
                                    pattern: sent,
                                    background_color: this.testBackground
                                        ? ([1, 3, 5].map((offset) =>
                                              Number.parseInt(
                                                  this.backgroundColor.slice(offset, offset + 2),
                                                  16,
                                              ),
                                          ) as [number, number, number])
                                        : undefined,
                                    frame,
                                    previous_token: token || null,
                                },
                            },
                            () => id === generation,
                            revision,
                        );
                        if (id !== generation) return;
                        token = next;
                    }
                    this.draw();
                    if (sent === 'marker' && !this.paused) this.frame = (frame + 1) % 256;
                    timer = setTimeout(() => void this.tick(id), sent === 'marker' ? 250 : 750);
                } catch (e) {
                    if (id === generation) await this.stop(String(e));
                }
            },
            open(event: Event) {
                (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
                this.deviceName =
                    (app().simulated ? 'Simulator · ' : '') + (app().device?.name || '');
                this.message = '';
                this.pattern = 'red';
                this.frame = 0;
                this.paused = false;
                device = app().deviceId;
                revision = app().snapshot.configuration_revision;
                until = Date.now() + 30000;
                this.running = true;
                (this.$refs.test as HTMLDialogElement).showModal();
                void this.tick(++generation);
            },
            async stop(message = '') {
                ++generation;
                clearTimeout(timer);
                this.running = false;
                this.message = message;
                const old = token;
                token = '';
                if (old)
                    await app()
                        .bridge.command('cancel_hardware_preview', { token: old })
                        .catch(() => {});
            },
            closed() {
                if (this.running && !(this.$refs.test as HTMLDialogElement).open) void this.stop();
            },
            close() {
                void this.stop();
                (this.$refs.test as HTMLDialogElement).close();
            },
            destroy() {
                void this.stop();
            },
        };
    });
}
