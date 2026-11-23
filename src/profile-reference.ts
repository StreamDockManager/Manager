import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { Profile, Manifest } from './types';
import { cloneJson } from './json-clone';
import { native } from './bridge';
import { open as chooseDirectory } from './window-dialogs';

export function registerProfileReference(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('profileReference', () => {
        let snapshot: Profile,
            model: Manifest,
            device: string,
            generation = 0;
        const urls: string[] = [];
        return {
            pages: [] as { id: string; name: string; file: string }[],
            selected: [] as string[],
            paper: 'A4',
            busy: false,
            writing: false,
            status: '',
            downloads: [] as { name: string; url: string }[],
            get filenames() {
                return this.pages.filter((p) => this.selected.includes(p.id)).map((p) => p.file);
            },
            open() {
                const s = app();
                if (!s.profile || !s.device) return;
                snapshot = cloneJson(s.profile);
                model = cloneJson(s.device.manifest);
                device = s.deviceId;
                this.pages = snapshot.pages.map((p, i) => ({
                    id: p.id,
                    name: p.name,
                    file: 'page-' + String(i + 1).padStart(2, '0') + '.png',
                }));
                this.selected = this.pages.map((p) => p.id);
                this.status = '';
                (this.$refs.reference as HTMLDialogElement).showModal();
            },
            release() {
                urls.splice(0).forEach(URL.revokeObjectURL);
                this.downloads = [];
            },
            close() {
                if (this.writing) return;
                ++generation;
                this.busy = false;
                this.release();
                (this.$refs.reference as HTMLDialogElement).close();
            },
            destroy() {
                ++generation;
                this.release();
            },
            async save() {
                if (this.busy || !this.selected.length) return;
                if (this.selected.length > 128) {
                    this.status = 'Select up to 128 pages for each reference.';
                    return;
                }
                if (import.meta.env.MODE === 'public-preview') {
                    this.status = 'Open Manager to save a local reference.';
                    return;
                }
                const id = ++generation,
                    ids = [...this.selected],
                    paper = this.paper;
                this.release();
                this.busy = true;
                this.status = 'Preparing reference…';
                const check = () => {
                    if (id !== generation) throw Error('Export cancelled.');
                };
                try {
                    const directory = native
                        ? await chooseDirectory({
                              directory: true,
                              title: 'Save reference in a new folder',
                          })
                        : null;
                    check();
                    if (native && !directory) {
                        this.status = '';
                        return;
                    }
                    const { renderReference } = await import('./reference-render');
                    const files = await renderReference(
                        snapshot,
                        model,
                        ids,
                        paper,
                        (b, controlId) =>
                            app().bridge.command('media_preview', {
                                asset: b.asset!,
                                deviceId: device,
                                controlId,
                                options: b.media,
                                previewBounds: [512, 512],
                            }),
                        check,
                    );
                    check();
                    if (native && typeof directory === 'string') {
                        const content: string[] = [];
                        for (const file of files) {
                            const base64 = await new Promise<string>((ok, fail) => {
                                const reader = new FileReader();
                                reader.onerror = () => fail(Error('Reference encoding failed.'));
                                reader.onload = () => ok(String(reader.result).split(',')[1]);
                                reader.readAsDataURL(new Blob([new Uint8Array(file.bytes)]));
                            });
                            content.push(base64);
                            check();
                        }
                        this.writing = true;
                        const path = await app().bridge.command('write_profile_reference', {
                            directory,
                            names: files.map((f) => f.name),
                            content,
                        });
                        this.status = 'Saved to ' + path;
                    } else {
                        this.downloads = files.map((f) => {
                            const url = URL.createObjectURL(
                                new Blob([new Uint8Array(f.bytes)], {
                                    type: 'image/png',
                                }),
                            );
                            urls.push(url);
                            return { name: f.name, url };
                        });
                        this.status = 'Reference ready to download.';
                    }
                } catch (e) {
                    if (id === generation) this.status = String(e);
                } finally {
                    if (id === generation) {
                        this.busy = false;
                        this.writing = false;
                    }
                }
            },
        };
    });
}
