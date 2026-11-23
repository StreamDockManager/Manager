export const bundledFonts = [{ id: 'sans', label: 'Inter' }];
// Resolve a configured font identifier to a font family
export function fontFamily(font = 'sans'): string {
    const name = font.startsWith('system:') ? font.slice(7) : '';
    return name &&
        new TextEncoder().encode(name).length <= 256 &&
        name.trim() === name &&
        !/\p{Cc}/u.test(name)
        ? name
        : 'Inter';
}
// Quote a font family safely for CSS use
export function cssFontFamily(font = 'sans'): string {
    return JSON.stringify(fontFamily(font));
}
// Build font choices while retaining selected unavailable families
export function fontChoices(families: string[], used: string[], loaded: boolean) {
    const choices = [
        ...bundledFonts,
        ...families.map((name) => ({ id: 'system:' + name, label: name })),
    ];
    const ids = new Set(choices.map((f) => f.id));
    const available = new Map(choices.map((f) => [f.label.toLowerCase(), f.label]));
    for (const id of used) {
        if (id && !ids.has(id)) {
            const family = fontFamily(id);
            const installed =
                id === 'mono'
                    ? 'Inter'
                    : id.startsWith('system:') && id.slice(7) === family
                      ? available.get(family.toLowerCase())
                      : undefined;
            choices.push({
                // Preserve imported IDs while matching the native case-insensitive resolver
                id,
                label: installed ?? family + (loaded ? ' (not installed)' : ' (loading…)'),
            });
            ids.add(id);
        }
    }
    return choices;
}
