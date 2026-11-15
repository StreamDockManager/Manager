/** Public demo isolation is build-time only; desktop and developer previews are unchanged. */
export const publicPreview = import.meta.env.MODE === 'public-preview';

if (publicPreview) {
    // A second CSP can only narrow the build's policy. Bind all requests to this
    // bundle's exact static prefix, including XHR/beacons/images and workers;
    // same-origin website authentication and API endpoints are not allowed
    const assetRoot = new URL(import.meta.env.BASE_URL, location.href).href;
    const policy = document.createElement('meta');
    policy.httpEquiv = 'Content-Security-Policy';
    policy.content = [
        "default-src 'none'",
        `script-src ${assetRoot} 'unsafe-inline'`,
        `style-src ${assetRoot} 'unsafe-inline'`,
        `img-src ${assetRoot} data: blob:`,
        `font-src ${assetRoot} data:`,
        `media-src ${assetRoot} data: blob:`,
        `connect-src ${assetRoot} data: blob:`,
        `worker-src ${assetRoot} blob:`,
        "frame-src 'none'",
        "object-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
    ].join('; ');
    document.head.append(policy);

    // The website embeds this document with an opaque-origin sandbox. Use fresh,
    // in-memory preferences even when opened directly: never read site storage
    const memoryStorage = (): Storage => {
        const data = new Map<string, string>();
        return {
            // Derive length from the current public preview state
            get length() {
                return data.size;
            },
            // Clear the retained public preview value
            clear() {
                data.clear();
            },
            // Get item for the public preview workflow
            getItem(key) {
                return data.get(String(key)) ?? null;
            },
            // Return the indexed memory-storage key or null when the index is absent
            key(index) {
                return [...data.keys()][index] ?? null;
            },
            // Remove the item
            removeItem(key) {
                data.delete(String(key));
            },
            // Set the item
            setItem(key, value) {
                data.set(String(key), String(value));
            },
        };
    };
    Object.defineProperty(window, 'localStorage', { value: memoryStorage() });
    Object.defineProperty(window, 'sessionStorage', { value: memoryStorage() });
    const request = window.fetch.bind(window);
    const base = new URL(import.meta.env.BASE_URL, location.href);
    // Complete the deferred public preview work using the current operation state
    window.fetch = (input, init) => {
        const raw = input instanceof Request ? input.url : String(input);
        const url = new URL(raw, location.href);
        const method = init?.method ?? (input instanceof Request ? input.method : 'GET');
        const asset = url.origin === base.origin && url.pathname.startsWith(base.pathname);
        const localMedia = url.protocol === 'data:' || url.protocol === 'blob:';
        if (!['GET', 'HEAD'].includes(method.toUpperCase()) || (!asset && !localMedia))
            return Promise.reject(new Error('The public demo only loads bundled preview assets.'));
        // No site cookies, OAuth state or backend APIs are available to the demo
        return request(input, { ...init, credentials: 'omit', redirect: 'error' });
    };
    window.open = () => null;
    // Allow only unmodified same-document fragment navigation and block other demo interactions
    const guardNavigation = (event: MouseEvent) => {
        const link = (event.target as Element | null)?.closest?.('a[href], input[type=file]');
        if (!link) return;
        let fragmentNavigation = false;
        if (link.tagName.toLowerCase() === 'a') {
            const href = link.getAttribute('href')?.trim() || '';
            const target = link.getAttribute('target')?.toLowerCase() || '';
            try {
                const current = new URL(location.href);
                const destination = new URL(href, current);
                fragmentNavigation =
                    !link.hasAttribute('download') &&
                    !link.hasAttribute('ping') &&
                    (!target || target === '_self') &&
                    !event.ctrlKey &&
                    !event.metaKey &&
                    !event.shiftKey &&
                    !event.altKey &&
                    (event.button === undefined || event.button === 0) &&
                    !destination.username &&
                    !destination.password &&
                    destination.origin === current.origin &&
                    destination.pathname === current.pathname &&
                    destination.search === current.search &&
                    (Boolean(destination.hash) || href === '#');
            } catch {
                // Malformed URLs are never navigation targets in the public demo
            }
        }
        if (!fragmentNavigation) event.preventDefault();
    };
    document.addEventListener('click', guardNavigation, true);
    document.addEventListener('auxclick', guardNavigation, true);
    document.addEventListener(
        'drop',
        // Respond to drop events in the public preview workflow
        (event) => {
            if (event.dataTransfer?.files.length) {
                event.preventDefault();
                event.stopImmediatePropagation();
            }
        },
        true,
    );
    document.addEventListener('submit', (event) => event.preventDefault(), true);
}
