import { defineConfig } from 'alpine-component-loader/project';
export default defineConfig({
    components: {
        directory: './public/components',
        manifest: './public/acl-manifest.json',
        inference: 'off',
        update: true,
    },
    routes: {
        manifest: './public/acl-manifest.json',
        outDir: './public/acl-routes',
        entries: [
            { key: 'plugin-logs', path: '/plugin-logs.html', groups: ['plugin-logs'] },
            { key: 'shell', path: '/__shell', groups: ['shell'] },
            { key: 'editor', path: '/', groups: ['editor'] },
            { key: 'action-library', path: '/action-library', groups: ['action-library'] },
            { key: 'macros', path: '/macros', groups: ['macros'] },
            { key: 'plugins', path: '/plugins', groups: ['plugins'] },
            { key: 'settings', path: '/settings', groups: ['settings'] },
            { key: 'diagnostics', path: '/diagnostics', groups: ['diagnostics'] },
        ],
    },
    contracts: {
        types: './generated/acl-components.d.ts',
        customElements: './generated/custom-elements.json',
    },
    watch: { tasks: ['manifest', 'types', 'routes'], debounce: 100 },
    vite: { moduleDelivery: 'copy', routeDirectory: 'acl-routes' },
});
