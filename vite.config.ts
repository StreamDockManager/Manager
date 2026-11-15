import { defineConfig, type UserConfig } from 'vite';
import { managerBuildVersion } from './scripts/build-version.ts';
import managerPackage from './package.json' with { type: 'json' };
import { alpineComponentLoader } from 'alpine-component-loader/vite';
import { readFile } from 'node:fs/promises';
import { clearAclRoutes } from './scripts/acl-routes.ts';
import { publicPreviewBundle } from './scripts/public-preview-build.ts';
import { productionUiAssets } from './scripts/ui-bundle.ts';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const aclPackageRoot = dirname(
    createRequire(import.meta.url).resolve('alpine-component-loader/package.json'),
);
const { version } = JSON.parse(await readFile(join(aclPackageRoot, 'package.json'), 'utf8'));
const aclModuleDirectory = `assets/alpine-component-loader/${version}`;

export default defineConfig(({ mode }): UserConfig => ({
    base: mode === 'public-preview' ? '/preview/app/' : '/',
    optimizeDeps: { include: ['@tauri-apps/plugin-dialog'] },
    plugins: [
        publicPreviewBundle(mode),
        managerBuildVersion(managerPackage.version, fileURLToPath(new URL('.', import.meta.url))),
        {
            name: 'stream-dock-manager-clear-acl-routes',
            buildStart: {
                order: 'pre',
                sequential: true,
                // Handle the incoming vite.config callback
                async handler() {
                    await clearAclRoutes(
                        fileURLToPath(new URL('./public/acl-routes/', import.meta.url)),
                    );
                },
            },
        },
        alpineComponentLoader({
            configFile: './acl.config.mjs',
            moduleDirectory: aclModuleDirectory,
        }),
        productionUiAssets(aclPackageRoot, aclModuleDirectory),
    ],
    server: {
        port: 1420,
        strictPort: false,
        preTransformRequests: false,
        allowedHosts: ['.ts.net'],
        watch: {
            // Captures can contain full source trees; generated files must not trigger reloads.
            ignored: [
                '**/artifacts/**',
                '**/.cache/**',
                '**/cache/**',
                '**/.npm-cache/**',
                '**/.tools/**',
                '**/target/**',
                '**/dist/**',
                '**/test-results/**',
                '**/playwright-report/**',
                '**/__pycache__/**',
                '**/.venv/**',
            ],
        },
    },
    clearScreen: false,
    build: {
        outDir: mode === 'public-preview' ? 'dist-public-preview' : 'dist',
        target: ['es2022', 'safari17'],
        rollupOptions: {
            input:
                mode === 'public-preview'
                    ? { main: resolve('index.html') }
                    : {
                          main: resolve('index.html'),
                          actionPopup: resolve('action-popup.html'),
                          pluginLogs: resolve('plugin-logs.html'),
                          configurationRecovery: resolve('config-recovery.html'),
                      },
        },
    },
}));
