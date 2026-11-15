import type { MediaOptions } from '../generated/runtime';
import type AlpineType from '@alpinejs/csp';
import { native } from './bridge';
import { releaseMediaPreview } from './media-preview';
import type { AppState } from './state';
import type { registerTheme } from './theme';
import { windowWork } from './window-work';
import { readWallpaperFrame, wallpaperDelay } from './wallpaper-frame';

// Register the theme wallpaper
export function registerThemeWallpaper(Alpine: typeof AlpineType, app: () => AppState) {
    // Resolve the current theme wallpaper appearance
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    // Complete the deferred theme wallpaper work using the current operation state
    Alpine.data('themeWallpaper', () => {
        const root = document.documentElement;
        const motion = matchMedia('(prefers-reduced-motion: reduce)');
        let observer: MutationObserver | undefined;
        let timer: ReturnType<typeof setTimeout> | undefined;
        let generation = 0;
        let pending = false;
        let token: string | null = null;
        let visible = true;
        // Update wallpaper visibility and immediately reconsider whether frames should refresh
        const visibilityChanged = (event: Event) => {
            visible = (event as CustomEvent<boolean>).detail;
            refresh();
        };
        // Refresh theme wallpaper data while keeping the view synchronized
        let refresh = () => {};
        return {
            image: '',
            // Refresh theme wallpaper data while keeping the view synchronized
            refresh: () => {},
            // Initialize theme wallpaper state and its required lifecycle hooks
            init() {
                if (!native) return;
                // Complete the deferred theme wallpaper work using the current operation state
                this.refresh = refresh = () => {
                    generation++;
                    clearTimeout(timer);
                    if (!theme().wallpaper || root.dataset.highContrast === 'true') this.clear();
                    void this.frame();
                };
                observer = new MutationObserver(this.refresh);
                observer.observe(root, {
                    attributes: true,
                    attributeFilter: ['data-high-contrast', 'data-reduced-motion'],
                });
                this.$watch('$store.theme.wallpaper', () => {
                    this.clear();
                    this.refresh();
                });
                document.addEventListener('visibilitychange', this.refresh);
                motion.addEventListener('change', this.refresh);
                window.addEventListener('resize', this.refresh);
                window.addEventListener('editor-window-visibility', visibilityChanged);
                window.addEventListener('editor-window-resumed', this.refresh);
                this.refresh();
            },
            // Clear the retained theme wallpaper value
            clear() {
                token = null;
                releaseMediaPreview(this.image);
                this.image = '';
            },
            // Process the current theme wallpaper frame
            async frame() {
                const wallpaper = theme().wallpaper;
                if (
                    !wallpaper ||
                    document.hidden ||
                    !visible ||
                    root.dataset.highContrast === 'true' ||
                    pending ||
                    windowWork.quiescing
                )
                    return;
                const still = motion.matches || root.dataset.reducedMotion === 'true';
                const revision = generation;
                const scale = Math.min(1, 1280 / Math.max(window.innerWidth, window.innerHeight));
                pending = true;
                let succeeded = false;
                let nextMs: number | null = null;
                const started = performance.now();
                try {
                    const bytes = await app().bridge.command('theme_wallpaper_frame', {
                        asset: wallpaper.asset_id,
                        options: wallpaper.options as MediaOptions,
                        size: [
                            Math.max(1, Math.round(window.innerWidth * scale)),
                            Math.max(1, Math.round(window.innerHeight * scale)),
                        ],
                        still,
                        previous: token,
                    });
                    if (revision !== generation) return;
                    const frame = readWallpaperFrame(bytes);
                    nextMs = frame.nextMs;
                    if (frame.pixels.length) {
                        const previous = this.image;
                        this.image = URL.createObjectURL(
                            new Blob([frame.pixels], { type: frame.mime! }),
                        );
                        releaseMediaPreview(previous);
                    } else if (frame.token !== token || !this.image) {
                        throw Error('Wallpaper reply omitted an unknown frame');
                    }
                    token = frame.token;
                    succeeded = true;
                } catch {
                    if (revision === generation) this.clear();
                } finally {
                    pending = false;
                    if (revision !== generation) void this.frame();
                    else if (succeeded && nextMs !== null && !still)
                        timer = setTimeout(
                            () => void this.frame(),
                            wallpaperDelay(nextMs, started, performance.now()),
                        );
                }
            },
            // Release theme wallpaper state and associated lifecycle resources
            destroy() {
                visible = false;
                generation++;
                clearTimeout(timer);
                observer?.disconnect();
                document.removeEventListener('visibilitychange', this.refresh);
                motion.removeEventListener('change', this.refresh);
                window.removeEventListener('resize', this.refresh);
                window.removeEventListener('editor-window-visibility', visibilityChanged);
                window.removeEventListener('editor-window-resumed', this.refresh);
                this.clear();
            },
        };
    });
}
