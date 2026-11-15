/** Preorder rows allow collapsed descendants to be skipped in one linear traversal. */
export function visibleMacroRows<T extends { id: string; depth: number }>(
    rows: T[],
    collapsed: string[],
) {
    const hidden = new Set(collapsed);
    const visible: (T & { expanded: boolean })[] = [];
    let collapsedDepth: number | undefined;
    for (const row of rows) {
        if (collapsedDepth !== undefined && row.depth > collapsedDepth) continue;
        collapsedDepth = undefined;
        const expanded = !hidden.has(row.id);
        visible.push({ ...row, expanded });
        if (!expanded) collapsedDepth = row.depth;
    }
    return visible;
}
