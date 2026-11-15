import type { PreviewHandlers } from './preview-handlers';
import { composePreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import { assertKeyboardMacroAction } from './keyboard-macro-validation';
import { previewBindingLookup } from './preview-binding-lookup';
import { resolvePaletteBinding } from './palette-binding';
import { previewSources, renderCompositionPreview } from './display-canvas';
import { version as managerVersion } from '../package.json';
import { renderPreviewDisplay, type PreviewVisual } from './plugin-display-preview';
import type { PluginPackageReview, BootLogoPreview } from '../generated/runtime';
import { emptyPluginHealth, emptyPluginRuntime } from './plugin-runtime';
// Deterministic in-memory fixtures. These commands never access files, HID, or host actions
import type {
    Snapshot,
    UpdateStatus,
    UpdatePreferences,
    Binding,
    SetupProgress,
    SetupCapabilityJob,
    SetupCapabilityRequest,
    ImportPreview,
    ImportRepair,
    PluginPreflight,
} from '../generated/runtime';
import { defaultMedia } from './bridge';
import { iconPreviewHandlers } from './icon-preview';
import { createMacroSharingPreview } from './macro-sharing-preview';
import { transparentBackground } from './background-color';
import { iconImage } from './icon-svg';
import { cssFontFamily } from './display-fonts';
// Escape text before embedding it in generated SVG markup
const escape = (s: string) =>
    s
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;');
// Decode a fixture image into a canvas at the requested or natural dimensions
async function fixtureMediaFrame(uri: string, width?: number, height?: number): Promise<string> {
    const image = new Image();
    image.src = uri;
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = width ?? image.naturalWidth;
    canvas.height = height ?? image.naturalHeight;
    canvas.getContext('2d')!.drawImage(image, 0, 0, canvas.width, canvas.height);
    return canvas.toDataURL('image/png');
}
// Render a simulated binding using the selected device surface
export async function previewBinding(s: Snapshot, args: Record<string, unknown>): Promise<string> {
    const binding = resolvePaletteBinding(args.binding as Binding);
    const control = s.devices
        .find((d) => d.id === args.deviceId)
        ?.manifest.controls.find((c) => c.id === args.controlId);
    const w = control?.image_width || 112,
        h = control?.image_height || 112;
    const a = binding.appearance;
    const size = (h * (a?.size || 18)) / 100;
    const x = a?.align === 'left' ? 4 : a?.align === 'right' ? w - 4 : w / 2;
    const anchor = a?.align === 'left' ? 'start' : a?.align === 'right' ? 'end' : 'middle';
    const y =
        a?.position === 'top'
            ? size + 3
            : a?.position === 'center'
              ? h / 2 + size / 3
              : h - size / 2;
    const feedback: Record<string, string> = {
        volume: '50%',
        mute: 'Sound on',
        application: 'Sample application',
        macro: 'Idle',
    };
    const text = feedback[binding.feedback || 'off'] || '';
    let icon = '';
    if (a?.icon_asset) {
        const dim = (Math.min(w, h) * a.icon_size) / 100;
        const iy =
            a.icon_position === 'top'
                ? 3
                : a.icon_position === 'bottom'
                  ? h - dim - 3
                  : (h - dim) / 2;
        const data = iconImage(a.icon_asset, a.foreground);
        icon = `<image x="${(w - dim) / 2}" y="${iy}" width="${dim}" height="${dim}" href="${escape(data)}"/>`;
    } else if (a?.icon) {
        const response = await fetch(
            import.meta.env.BASE_URL + 'appearance/icons/' + encodeURIComponent(a.icon) + '.svg',
        );
        if (response.ok) {
            const svg = await response.text();
            const content = svg.slice(svg.indexOf('>') + 1, svg.lastIndexOf('</svg>'));
            const dim = (Math.min(w, h) * (a.icon_size || 42)) / 100;
            const iy =
                a.icon_position === 'top'
                    ? 3
                    : a.icon_position === 'bottom'
                      ? h - dim - 3
                      : (h - dim) / 2;
            icon = `<g transform="translate(${(w - dim) / 2},${iy}) scale(${dim / 24})" fill="none" stroke="${escape(a.foreground)}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${content}</g>`;
        }
    }
    const title =
        a?.show_title === false
            ? ''
            : `<text x="${x}" y="${y}" text-anchor="${anchor}" font-size="${size}" fill="${escape(a?.foreground || '#ffffff')}">${escape(binding.title)}</text>`;
    const media = args.mediaImage
        ? `<image width="${w}" height="${h}" preserveAspectRatio="none" href="${escape(String(args.mediaImage))}"/>`
        : '';
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" font-family="${escape(cssFontFamily(a?.font))}"><rect width="100%" height="100%" fill="${escape(binding.color)}"/>${media}${args.displayBody || ''}${icon}${title}${text ? `${transparentBackground(binding.color) ? '' : `<rect y="${h - size - 8}" width="100%" height="${size + 8}" fill="#000"/>`}<text x="${w / 2}" y="${h - 4}" text-anchor="middle" font-size="${size}" fill="#fff">${text}</text>` : ''}</svg>`;
    return 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
}
// Create isolated in-memory feature command handlers for browser preview
export function createFeaturePreview() {
    const bootLogos = new Map<string, BootLogoPreview>();
    const macroSharing = createMacroSharingPreview();
    const mediaImages = new Map<string, string>();
    const mediaMetadata = new Map<
        string,
        { format: string; animated: boolean; duration_ms: number }
    >();
    let textWarnings: string[] = [];
    const pluginDisplays = new Map<
        string,
        { visual: PreviewVisual; action: Binding['action']; scope: string }
    >();
    let updates: UpdateStatus = {
        effective_plugins_catalog_url: 'https://plugins.example.org/catalog.json',
        available_catalogs: [],
        host_os: 'linux',
        host_arch: 'amd64',
        installed_manager: managerVersion,
        plugin_updates: [],
        preferences: {
            automatic_checks: false,
            manager_repository: 'preview/manager',
            plugins_repository: 'StreamDockManager/Plugins',
            plugins_catalog_source: 'repository',
            plugin_channel: 'stable',
        },
        manager: {
            state: 'unavailable',
            error: null,
            last_success: null,
            retry_after: null,
            catalog: null,
            notified_versions: [],
        },
        plugins: {
            state: 'unavailable',
            error: null,
            last_success: null,
            retry_after: null,
            catalog: null,
            notified_versions: [],
        },
        jobs: [],
    };
    let progress: SetupProgress = {
        step: 0,
        complete: false,
        dismissed: false,
        new_install: false,
    };
    let staged: ImportPreview | null = null;
    const pluginReviews = new Map<string, PluginPackageReview>();
    const setupChecks = new Map<string, SetupCapabilityJob>();
    const setupDeadlines = new Map<string, number>();
    const sample: PluginPreflight = {
        id: 'preview.clock',
        name: 'Sample Clock',
        version: '1.0',
        source: 'preview://VSD Craft/plugins/clock.sdPlugin',
        kind: 'browser',
        runtime: null,
        installed: false,
        can_install: true,
        can_enable: true,
        checks: [
            {
                code: 'preview',
                level: 'unknown',
                message: 'Sample package for browser preview',
                remedy: 'Use the desktop app to inspect real plugin files.',
            },
        ],
    };
    const missingRuntime =
        typeof location !== 'undefined'
            ? new URLSearchParams(location.search).get('missingRuntime')
            : null;
    if (missingRuntime === 'nodejs') {
        const line = '24';
        sample.kind = missingRuntime;
        sample.can_enable = false;
        sample.runtime = {
            recommended_line: line,
            requirement: {
                kind: missingRuntime,
                channel: 'lts',
                version: line,
            },
            selected: null,
            state: 'missing',
            message: 'A compatible runtime is required.',
            overridden: false,
        };
        sample.checks.push({
            code: 'runtime_unavailable',
            level: 'blocker',
            message: 'A compatible runtime is required.',
            remedy: 'Install or select a supported runtime.',
        });
    }
    return (
        workspace: PreviewWorkspace,
        notifyPluginFeedback: (deviceId: string) => void,
        enrichPreflight: (report: PluginPreflight) => PluginPreflight,
    ) => {
        const { s, commit, markEdit } = workspace;
        // Validate and render the simulated binding with preview diagnostics
        const bindingDetails: NonNullable<PreviewHandlers['preview_binding_details']> = async (
            args,
        ) => {
            const binding = resolvePaletteBinding(args.binding as Binding);
            assertKeyboardMacroAction(binding.action, true);
            const mediaFixture = mediaImages.get(binding.asset || '');
            const device = s.devices.find((d) => d.id === args.deviceId);
            const { session, profile } = previewBindingLookup(s, device, String(args.controlId));
            const control = device?.manifest.controls.find((c) => c.id === args.controlId);
            const mediaImage = mediaFixture
                ? await fixtureMediaFrame(
                      mediaFixture,
                      control?.image_width || 112,
                      control?.image_height || 112,
                  )
                : '';
            let image = await previewBinding(s, { ...args, binding, mediaImage });
            let sourceVisual: PreviewVisual | undefined;
            let placeholder = false;
            let details = {
                image,
                availability: 'off',
                elements: [],
                states: [],
                current_state: null,
            } as import('../generated/runtime').BindingPreview;
            if (binding.action.kind === 'plugin') {
                const plugin = s.plugins.find(
                    (p) =>
                        p.id ===
                        (binding.action as Extract<Binding['action'], { kind: 'plugin' }>)
                            .plugin_id,
                );
                const fixture = pluginDisplays.get(JSON.stringify([args.deviceId, args.controlId]));
                details.availability =
                    binding.disabled || !plugin?.enabled ? 'inactive' : 'waiting';
                if (
                    details.availability !== 'inactive' &&
                    fixture &&
                    JSON.stringify(fixture.action) === JSON.stringify(binding.action) &&
                    fixture.scope ===
                        JSON.stringify([profile?.id, session?.page_id || profile?.active_page])
                ) {
                    sourceVisual = fixture.visual;
                    const rendered = renderPreviewDisplay(
                        fixture.visual,
                        binding,
                        control?.image_width || 112,
                        control?.image_height || 112,
                        args.previewState == null ? undefined : Number(args.previewState),
                    );
                    const overlay = await previewBinding(s, {
                        ...args,
                        mediaImage,
                        displayBody: rendered.body,
                        binding: { ...binding, title: binding.appearance ? binding.title : '' },
                    });
                    image = overlay;
                    details = {
                        ...rendered.details,
                        image,
                        availability:
                            fixture.visual.layout || fixture.visual.title ? 'live' : 'image_only',
                    };
                }
            }
            if (!sourceVisual && binding.action.kind === 'plugin') {
                const plugin = s.plugins.find(
                    (p) =>
                        p.id ===
                        (binding.action as Extract<Binding['action'], { kind: 'plugin' }>)
                            .plugin_id,
                );
                const capability = plugin?.capabilities?.actions[binding.action.action_id];
                if (capability?.feedback_layout) {
                    placeholder = true;
                    const weather = capability.feedback_layout.includes('weather-');
                    const mode = binding.action.action_id.split('.').at(-1) || 'conditions';
                    const wide =
                        (control?.image_width || 112) > (control?.image_height || 112) * 1.3;
                    const keys = weather
                        ? mode === 'forecast'
                            ? ['low', 'high']
                            : mode === 'sun'
                              ? ['sunrise', 'sunset']
                              : ['temperature']
                        : ['live_text'];
                    sourceVisual = {
                        plugin_id: binding.action.plugin_id,
                        action_id: binding.action.action_id,
                        layout: {
                            id: weather
                                ? `weather-${mode}-${wide ? 'wide' : 'key'}-v1`
                                : 'plugin-display',
                            items: [
                                {
                                    key: 'icon',
                                    label: 'Plugin icon',
                                    type: 'pixmap',
                                    rect: wide ? [2, 4, 72, 88] : [55, 0, 90, 43],
                                    value:
                                        plugin?.actions.find(
                                            (a) =>
                                                a.id ===
                                                (
                                                    binding.action as Extract<
                                                        Binding['action'],
                                                        { kind: 'plugin' }
                                                    >
                                                ).action_id,
                                        )?.icon || '',
                                },
                                ...keys.map((key, i) => ({
                                    key,
                                    label: key,
                                    type: 'text',
                                    rect: wide
                                        ? [78, 8 + (i * 84) / keys.length, 120, 84 / keys.length]
                                        : [4, 46 + (i * 48) / keys.length, 192, 48 / keys.length],
                                    value: '—',
                                })),
                            ],
                        },
                    };
                    const rendered = renderPreviewDisplay(
                        sourceVisual,
                        binding,
                        control?.image_width || 112,
                        control?.image_height || 112,
                    );
                    details.elements = rendered.details.elements;
                }
            }
            details.sources = previewSources(
                binding,
                sourceVisual,
                details.elements,
                placeholder,
                mediaImage,
            );
            details.placeholder = placeholder;
            if (binding.composition)
                details.image = renderCompositionPreview(
                    binding,
                    details.sources,
                    control?.image_width || 112,
                    control?.image_height || 112,
                    args.previewState == null ? sourceVisual?.state : Number(args.previewState),
                );
            details.text_warnings = [...textWarnings];
            return details;
        };
        // Stage only recognized sample plugin packages for browser review
        const packageReviews = (paths: string[]) =>
            paths.map((path, index) => {
                if (path !== sample.source)
                    throw Error('Browser preview accepts only the sample package.');
                const review: PluginPackageReview = {
                    id: 'preview-plugin-' + index,
                    trusted: false,
                    package: {
                        files: {},
                        os: 'preview',
                        id: sample.id,
                        version: sample.version,
                        path,
                        sha256: '',
                        size: 0,
                        architectures: [],
                        minimum_manager: '0.1.0',
                        required_capabilities: [],
                    },
                    installed_version: s.plugins.find((p) => p.id === sample.id)?.version || null,
                    channel: 'local',
                    channel_change: false,
                    compatible: true,
                    reason: null,
                    manifest: {
                        id: sample.id,
                        name: sample.name,
                        version: sample.version,
                        publisher: null,
                        dialect: 'vsd',
                        entry_point: 'preview.html',
                        runtime_kind: sample.kind,
                        platforms: [],
                        action_count: 0,
                        control_types: [],
                    },
                    source: {
                        format: 'folder',
                        filename: 'clock.sdPlugin',
                        sha256: 'preview-only',
                    },
                    compatibility: structuredClone(sample),
                    can_import: true,
                    can_run: sample.can_enable,
                    version_change: null,
                };
                pluginReviews.set(review.id, review);
                return review;
            });
        return composePreviewHandlers(macroSharing(workspace), {
            // Stage a fixture boot-logo preview without touching hardware
            prepare_boot_logo: async (args) => {
                const device = s.devices.find((device) => device.id === args.deviceId);
                const size = device?.manifest.capabilities.boot_logo?.size;
                if (!device?.connected || !size)
                    throw new Error('Device does not support startup logos');
                if (!['contain', 'cover'].includes(String(args.fit)))
                    throw new Error('Invalid image sizing');
                const canvas = document.createElement('canvas');
                [canvas.width, canvas.height] = size;
                const context = canvas.getContext('2d')!;
                context.fillStyle = '#111111';
                context.fillRect(0, 0, ...size);
                context.fillStyle = '#eeeeee';
                context.font = `${size[1] / 10}px sans-serif`;
                context.textAlign = 'center';
                context.fillText('Startup preview', size[0] / 2, size[1] / 2);
                const preview: BootLogoPreview = {
                    id: crypto.randomUUID(),
                    device_id: device.id,
                    file_name: String(args.path).split('/').pop()!,
                    size,
                    fit: String(args.fit),
                    image: canvas.toDataURL('image/png'),
                    simulated: true,
                };
                bootLogos.set(preview.id, preview);
                return preview;
            },
            // Discard the staged browser boot-logo preview
            cancel_boot_logo: async (args) => {
                bootLogos.delete(String(args.id));
                return undefined;
            },
            // Accept the staged boot logo in simulated device state
            commit_boot_logo: async (args) => {
                const preview = bootLogos.get(String(args.id));
                if (!preview) throw new Error('Startup-logo preview expired');
                if (
                    !s.devices.some((device) => device.id === preview.device_id && device.connected)
                )
                    throw new Error('Device changed');
                bootLogos.delete(preview.id);
                return {
                    device_id: preview.device_id,
                    sent: false,
                    message: 'Simulator preview updated.',
                };
            },
            // Return no installed fonts because browser preview cannot enumerate host fonts
            system_fonts: async (args) => {
                return []; // Installed-font discovery belongs to the desktop host
            },
            // Return an independent copy of simulated update status
            update_status: async (args) => {
                return structuredClone(updates);
            },
            ...iconPreviewHandlers,
            // Update the in-memory preview update preferences
            save_update_preferences: async (args) => {
                updates.preferences = structuredClone(args.preferences as UpdatePreferences);
                updates.effective_plugins_catalog_url =
                    updates.preferences.plugins_catalog_source === 'repository'
                        ? `https://api.github.com/repos/${updates.preferences.plugins_repository}/releases`
                        : updates.preferences.plugins_catalog_url ||
                          'https://plugins.example.org/catalog.json';
                return undefined;
            },
            // Report that native GitHub credentials are unavailable in browser preview
            plugin_github_credential_status: async (args) => {
                return 'missing' as const;
            },
            // Reject credential storage outside the native application
            save_plugin_github_token: async (args) => {
                throw Error('The OS credential store is unavailable in browser preview.');
            },
            // Reject native credential removal from browser preview
            clear_plugin_github_token: async (args) => {
                throw Error('The OS credential store is unavailable in browser preview.');
            },
            // Simulate an update check for a recognized product
            check_updates: async (args) => {
                const product = String(args.product);
                if (product !== 'manager' && product !== 'plugins') throw Error('Invalid product');
                updates[product] = {
                    ...updates[product],
                    state: 'current',
                    last_success: 1792670400,
                    error: null,
                };
                return 'preview-check-' + product;
            },
            // Reject native release downloads in browser preview
            download_release_asset: async (args) => {
                throw Error('Downloads require the desktop application.');
            },
            // Require browser-visible release links instead of native link opening
            open_manager_release_link: async (args) => {
                throw Error('Use the release links to open GitHub in browser preview.');
            },
            // Require browser-visible documentation links instead of native link opening
            open_plugin_documentation: async (args) => {
                throw Error('Use the documentation links in browser preview.');
            },
            // Acknowledge cancellation without a native download job
            cancel_update_job: async (args) => {
                return undefined;
            },
            // Create a simulated setup-capability check
            start_setup_capabilities: async (args) => {
                const request = structuredClone(args.request as SetupCapabilityRequest);
                const job: SetupCapabilityJob = {
                    id: crypto.randomUUID(),
                    request,
                    state: 'running',
                    checks: [],
                    qualification:
                        'Browser fixtures do not establish native readiness or physical qualification.',
                };
                if (setupChecks.size >= 8) {
                    const oldest = setupChecks.keys().next().value!;
                    setupChecks.delete(oldest);
                    setupDeadlines.delete(oldest);
                }
                setupChecks.set(job.id, job);
                setupDeadlines.set(job.id, Date.now() + 30_000);
                return structuredClone(job);
            },
            // Advance and report the requested simulated setup check
            setup_capability_status: async (args) => {
                const job = setupChecks.get(String(args.id));
                if (!job) throw Error('Setup check is no longer retained');
                if (job.state === 'running' && job.request.mode === 'passive') {
                    job.state = 'complete';
                    job.checks = [
                        {
                            id: 'preview',
                            title: 'Browser sample readiness',
                            status: 'unverified',
                            detail: 'Native helpers, dependencies, permissions and device input require the desktop application.',
                            action: '',
                        },
                    ];
                } else if (job.state === 'running' && Date.now() >= setupDeadlines.get(job.id)!) {
                    job.state = 'failed';
                    job.checks = [
                        {
                            id: 'input',
                            title: 'Input check expired',
                            status: 'unavailable',
                            detail: 'Browser fixtures cannot verify native dock input.',
                            action: '',
                        },
                    ];
                }
                return structuredClone(job);
            },
            // Cancel the matching simulated setup check
            cancel_setup_capabilities: async (args) => {
                const job = setupChecks.get(String(args.id));
                if (job) job.state = 'cancelled';
                return undefined;
            },
            // Report simulated setup progress and available checks
            setup_status: async (args) => {
                return {
                    progress,
                    checks: [
                        {
                            id: 'device',
                            title: 'Browser simulator',
                            status: 'ok',
                            detail: 'Sample environment; host actions stay disabled.',
                            action: '',
                        },
                        {
                            id: 'ffmpeg',
                            title: 'Optional video fallback (FFmpeg)',
                            status: 'optional',
                            detail: 'Desktop video imports use supported browser codecs, with optional FFmpeg fallback.',
                            action: 'recheck',
                        },
                    ],
                };
            },
            // Replace the in-memory setup progress snapshot
            save_setup_progress: async (args) => {
                progress = structuredClone(args.progress as SetupProgress);
                return undefined;
            },
            // Install a supplied plugin display fixture into preview state
            preview_plugin_display_fixture: async (args) => {
                if (args.plugin) {
                    const plugin = structuredClone(args.plugin as Snapshot['plugins'][number]);
                    s.plugins = [...s.plugins.filter((p) => p.id !== plugin.id), plugin];
                }
                if (!args.visual) {
                    if (args.plugin) commit();
                    notifyPluginFeedback(args.deviceId);
                    return;
                }
                const device = s.devices.find((d) => d.id === args.deviceId);
                const { session, profile, binding } = previewBindingLookup(
                    s,
                    device,
                    String(args.controlId),
                );
                if (!binding) throw Error('Save a fixture binding first');
                pluginDisplays.set(JSON.stringify([args.deviceId, args.controlId]), {
                    visual: structuredClone(args.visual as PreviewVisual),
                    action: structuredClone(binding.action),
                    scope: JSON.stringify([profile?.id, session?.page_id || profile?.active_page]),
                });
                if (args.plugin) commit();
                notifyPluginFeedback(args.deviceId);
                return;
            },
            // Register a media image and metadata for fixture rendering
            preview_media_fixture: async (args) => {
                mediaImages.set(String(args.asset), String(args.image));
                mediaMetadata.set(String(args.asset), {
                    format: String(args.format || 'png'),
                    animated: Boolean(args.animated),
                    duration_ms: Number(args.duration_ms || 0),
                });
                return undefined;
            },
            // Retain a bounded set of fixture text warnings
            preview_text_checks_fixture: async (args) => {
                textWarnings = (args.warnings as string[]).slice(0, 64);
                return undefined;
            },
            // Return metadata for a recognized preview media asset
            media_details: async (args) => {
                const metadata = mediaMetadata.get(String(args.asset));
                if (!metadata) throw Error('Media is not available in this browser fixture.');
                return metadata;
            },
            // Render the simulated binding and return its image
            preview_binding: async (args) => (await bindingDetails(args)).image,
            preview_binding_details: bindingDetails,
            // Report simulated firmware support for the selected device
            firmware_status: async (args) => {
                const supported =
                    s.devices.find((d) => d.id === args.deviceId)?.manifest.capabilities
                        .firmware_query === true;
                return {
                    state: supported ? 'not_checked' : 'unsupported',
                    installed: null,
                    installed_source: null,
                    latest: null,
                    latest_state: supported ? 'not_checked' : 'unsupported',
                    latest_message: '',
                    latest_source: null,
                    latest_checked: null,
                    latest_stale: false,
                    last_checked: null,
                    instructions: supported
                        ? 'https://www.vsdinside.com/blogs/tutorial/vsd-stream-dock-connection-issues'
                        : null,
                    message:
                        'Simulator fixture: firmware is unmeasured; no device or network queries are made.',
                    stale: false,
                };
            },
            // Simulate a firmware check without contacting or updating hardware
            check_firmware: async (args) => {
                const supported =
                    s.devices.find((d) => d.id === args.deviceId)?.manifest.capabilities
                        .firmware_query === true;
                return {
                    state: supported ? 'not_checked' : 'unsupported',
                    installed: null,
                    installed_source: null,
                    latest: null,
                    latest_state: supported ? 'not_checked' : 'unsupported',
                    latest_message: '',
                    latest_source: null,
                    latest_checked: null,
                    latest_stale: false,
                    last_checked: null,
                    instructions: supported
                        ? 'https://www.vsdinside.com/blogs/tutorial/vsd-stream-dock-connection-issues'
                        : null,
                    message:
                        'Simulator fixture: firmware is unmeasured; no device or network queries are made.',
                    stale: false,
                };
            },
            // Acknowledge firmware instructions without invoking native services
            open_firmware_instructions: async (args) => {
                return undefined;
            },
            // Render a known preview media asset
            media_preview: async (args) => {
                const image = mediaImages.get(args.asset);
                if (!image) throw Error('Media is not available in this browser fixture.');
                const png = await fixtureMediaFrame(image);
                const bytes = new Uint8Array(await (await fetch(png)).arrayBuffer());
                const metadata = new TextEncoder().encode(
                    JSON.stringify({
                        max_fps: 24,
                        animated: false,
                        requested_tps: 0,
                        duration_ms: 0,
                        ...mediaMetadata.get(args.asset),
                        mime: 'image/png',
                    }),
                );
                const buffer = new ArrayBuffer(4 + metadata.length + bytes.length);
                new DataView(buffer).setUint32(0, metadata.length, true);
                new Uint8Array(buffer, 4, metadata.length).set(metadata);
                new Uint8Array(buffer, 4 + metadata.length).set(bytes);
                return buffer;
            },
            // Deny native accessibility authorization in browser preview
            authorize_accessibility: async (args) => {
                return false;
            },
            // Produce nonmatching rule results without inspecting real desktop state
            evaluate_rules: async (args) => {
                return s.rules
                    .filter((r) => r.device_key === args.deviceId)
                    .map((r) => ({
                        id: r.id,
                        matched: false,
                        winner: false,
                        reasons: [
                            'Browser preview: use the native app to evaluate current desktop conditions.',
                        ],
                    }));
            },
            // Simulate installed-plugin discovery for a supported source
            scan_plugin_sources: async (args) => {
                if (!['all', 'vsd', 'opendeck'].includes(String(args.source || 'all')))
                    throw Error(
                        'Unsupported discovery source. Choose VSD Craft, OpenDeck, or a custom folder.',
                    );
                if (args.source === 'opendeck')
                    return [
                        {
                            ...sample,
                            id: 'org.openaction.preview',
                            name: 'OpenAction sample',
                            source: 'preview://OpenDeck/plugins/org.openaction.preview.sdPlugin',
                            kind: 'browser',
                            installed: s.plugins.some((p) => p.id === 'org.openaction.preview'),
                        },
                    ];
                return [
                    { ...sample, installed: s.plugins.some((p) => p.id === sample.id) },
                    {
                        ...sample,
                        id: 'preview.builtin',
                        name: 'Sample VSD built-in',
                        source: 'preview://VSD Craft/plugins/builtin.sdPlugin',
                        kind: 'vendor_builtin',
                        can_install: false,
                        can_enable: false,
                        checks: [
                            {
                                code: 'vendor_builtin',
                                level: 'blocker',
                                message: 'Requires VSD Craft: no standalone entry point',
                                remedy: 'Use an equivalent Manager action.',
                            },
                        ],
                    },
                ];
            },
            // Update simulated plugin sleep preferences
            set_plugin_sleep: async (args) => {
                const plugin = s.plugins.find((p) => p.id === args.id);
                if (!plugin) throw Error('Plugin missing');
                plugin.sleep_when_hidden = Boolean(args.sleepWhenHidden);
                return commit().snapshot;
            },
            // Update the simulated plugin enabled state
            enable_plugin: async (args) => {
                const plugin = s.plugins.find((p) => p.id === args.id);
                if (!plugin) throw Error('Plugin missing');
                plugin.enabled = Boolean(args.enabled);
                plugin.status = plugin.enabled
                    ? 'Preview enabled — code does not run'
                    : 'Preview disabled';
                return commit().snapshot;
            },
            // Return enriched preflight information for a fixture plugin
            preflight_plugin: async (args) => {
                const plugin = s.plugins.find((p) => p.id === args?.id);
                return enrichPreflight({
                    ...sample,
                    id: plugin?.id || sample.id,
                    name: plugin?.name || sample.name,
                    source: String(args?.path || sample.source),
                });
            },
            // Prepare a review for the recognized sample plugin package
            prepare_plugin_package: async (args) => packageReviews([args.path])[0],
            // Prepare reviews for sample plugin selections without installing them
            import_plugin_selection: async (args) => ({
                reviews: packageReviews(args.paths),
                results: [],
            }),
            // Discard a staged sample plugin review
            cancel_plugin_package: async (args) => {
                pluginReviews.delete(String(args.id));
                return undefined;
            },
            // Commit a reviewed sample plugin into preview state
            commit_plugin_package: async (args) => {
                const review = pluginReviews.get(String(args.id));
                if (!review) throw Error('Package review expired');
                if (!s.plugins.some((p) => p.id === sample.id))
                    s.plugins.push({
                        id: sample.id,
                        name: sample.name,
                        version: sample.version,
                        dialect: 'vsd',
                        kind: sample.kind,
                        enabled: false,
                        sleep_when_hidden: true,
                        status: 'Preview only — disabled',
                        appearance: 'follow-ui',
                        actions: [],
                        runtime: emptyPluginRuntime(),
                        health: emptyPluginHealth(),
                    });
                pluginReviews.delete(review.id);
                return commit().snapshot;
            },
            // Return no pending native plugin setup tasks
            pending_plugin_profile_setup: async (args) => {
                return [];
            },
            // Acknowledge setup without changing a native installation
            acknowledge_plugin_profile_setup: async (args) => {
                return undefined;
            },
            // List profiles associated with the selected preview plugin
            list_plugin_profiles: async (args) => {
                const plugin = s.plugins.find((plugin) => plugin.id === args.id);
                if (!plugin) throw Error('Plugin is no longer installed');
                return {
                    plugin_id: plugin.id,
                    name: plugin.name,
                    fingerprint: 'preview',
                    can_configure: !!plugin.capabilities?.plugin_settings,
                    suggestions: [],
                    warnings: [],
                };
            },
            // Stage a sample profile import against the current preview revision
            prepare_profile_import: async (args) => {
                staged = {
                    id: 'sample-import',
                    expected_revision: s.configuration_revision,
                    mode: 'merge',
                    profiles: ['Sample imported profile'],

                    style_count: 0,
                    issues: [],
                    repair_suggestions: [],
                    recipe_warning: null,
                    impact: ['Sample archive in memory. Existing profiles are preserved.'],
                };
                return structuredClone(staged);
            },
            // Remove the selected in-memory import recipe
            forget_import_recipe: async (args) => {
                throw Error('Remembered repair recipes require the native application.');
            },
            // Update the staged preview import selection
            update_profile_import: async (args) => {
                if (args.remember)
                    throw Error('Remembered repair recipes require the native application.');
                if (!staged) throw Error('Import session expired');
                staged.mode = String(args.mode);
                staged.expected_revision = s.configuration_revision;
                for (const repair of args.repairs as ImportRepair[])
                    staged.issues = staged.issues.filter((i) => i.key !== repair.key);
                staged.impact = [
                    staged.mode === 'replace'
                        ? 'Replace existing profiles with the sample profile.'
                        : 'Merge the sample profile alongside existing profiles.',
                ];
                return structuredClone(staged);
            },
            // Discard the staged preview profile import
            cancel_profile_import: async (args) => {
                staged = null;
                return undefined;
            },
            // Apply the staged profile import to the browser workspace
            commit_profile_import: async (args) => {
                if (!staged || args.expectedRevision !== s.configuration_revision)
                    throw Error('Workspace changed. Recheck this import before committing.');
                const profile = s.profiles[0]
                    ? structuredClone(s.profiles[0])
                    : {
                          id: '',
                          name: '',
                          model_id: s.devices[0]?.model_id || 'n4-pro',
                          active_page: 'main',
                          pages: [{ id: 'main', name: 'Main page', bindings: {} }],
                          background_media: defaultMedia(),
                      };
                profile.id = crypto.randomUUID();
                profile.name = 'Sample imported profile';
                if (staged.mode === 'replace') {
                    s.profiles = [profile];
                    s.active_profile = profile.id;
                    s.rules = [];
                } else {
                    if (!s.profiles.length) s.active_profile = profile.id;
                    s.profiles.push(profile);
                }
                staged = null;
                markEdit();
                return commit().snapshot;
            },
        } satisfies PreviewHandlers);
    };
}
