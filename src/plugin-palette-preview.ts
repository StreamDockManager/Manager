/** Plugin theme previews are global, not gated on a physical dock or its power state.
 * The caller supplies only window/route lifetime: browser builds never get a lease.
 */
export function pluginPalettePreviewContext(
    native: boolean,
    state: {
        route: string;
        selection?: string;
        followNavigation?: boolean;
        suspended: boolean;
        hidden: boolean;
        quiescing: boolean;
        closing: boolean;
    },
) {
    if (!native || state.suspended || state.hidden || state.quiescing || state.closing) return null;
    return {
        key: state.followNavigation
            ? 'palette-browser'
            : JSON.stringify([state.route, state.selection]),
    };
}
