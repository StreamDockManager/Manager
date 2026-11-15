export interface JsonDiffLine {
    kind: 'same' | 'removed' | 'added';
    text: string;
    before: number | null;
    after: number | null;
}
/** Bounded line diff; large changed blocks remain an honest remove/add block. */
export function diffJson(
    before: string,
    after: string,
): { lines: JsonDiffLine[]; coarse: boolean; truncated: boolean } {
    const a = before.split('\n'),
        b = after.split('\n');
    let prefix = 0;
    while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
    let suffix = 0;
    while (
        suffix < a.length - prefix &&
        suffix < b.length - prefix &&
        a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
    )
        suffix++;
    const rows = a.length - prefix - suffix,
        columns = b.length - prefix - suffix;
    const coarse = (rows + 1) * (columns + 1) > 250000;
    const lines: JsonDiffLine[] = [];
    let truncated = false,
        characters = 0;
    // Append the new entry to the recovery json diff collection
    const append = (
        kind: JsonDiffLine['kind'],
        text: string,
        before: number | null,
        after: number | null,
    ) => {
        if (lines.length >= 2000 || characters + text.length > 500000) {
            truncated = true;
            return;
        }
        lines.push({ kind, text, before, after });
        characters += text.length;
    };
    if (prefix > 3) append('same', `… ${prefix - 3} unchanged lines …`, null, null);
    for (let i = Math.max(0, prefix - 3); i < prefix; i++) append('same', a[i], i + 1, i + 1);
    if (coarse) {
        for (let i = prefix; i < a.length - suffix; i++) append('removed', a[i], i + 1, null);
        for (let j = prefix; j < b.length - suffix; j++) append('added', b[j], null, j + 1);
    } else {
        const width = columns + 1,
            table = new Uint32Array((rows + 1) * width);
        for (let i = rows - 1; i >= 0; i--)
            for (let j = columns - 1; j >= 0; j--)
                table[i * width + j] =
                    a[prefix + i] === b[prefix + j]
                        ? 1 + table[(i + 1) * width + j + 1]
                        : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
        let i = 0,
            j = 0;
        while (i < rows || j < columns) {
            if (i < rows && j < columns && a[prefix + i] === b[prefix + j]) {
                append('same', a[prefix + i], prefix + i + 1, prefix + j + 1);
                i++;
                j++;
            } else if (
                i < rows &&
                (j === columns || table[(i + 1) * width + j] >= table[i * width + j + 1])
            ) {
                append('removed', a[prefix + i], prefix + i + 1, null);
                i++;
            } else {
                append('added', b[prefix + j], null, prefix + j + 1);
                j++;
            }
        }
    }
    for (let i = suffix; i > Math.max(0, suffix - 3); i--)
        append('same', a[a.length - i], a.length - i + 1, b.length - i + 1);
    if (suffix > 3) append('same', `… ${suffix - 3} unchanged lines …`, null, null);
    return { lines, coarse, truncated };
}
