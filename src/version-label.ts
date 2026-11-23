// Label development execution without changing package or Git revision provenance
export function versionLabel(
    base: string,
    native: boolean,
    nativeDevelopment: boolean | undefined,
    browserDevelopment: boolean,
): string {
    return base + ((native ? nativeDevelopment === true : browserDevelopment) ? '+dev' : '');
}
