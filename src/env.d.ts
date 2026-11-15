declare module '@alpinejs/csp' {
    import Alpine from 'alpinejs';
    export default Alpine;
}

/// <reference types="alpine-component-loader/vite" />
declare module 'virtual:alpine-component-loader/routes' {
    export function registerRoute(name: string): Promise<unknown>;
}

declare module 'virtual:manager-build-version' {
    const version: string;
    export default version;
}
