// Clamp the requested page and return its item window and display bounds
export function paginate<T>(items: T[], requestedPage: number, pageSize: number) {
    const size = Math.max(1, Math.floor(pageSize));
    const pages = Math.max(1, Math.ceil(items.length / size));
    const page = Math.min(pages, Math.max(1, Math.floor(requestedPage)));
    const offset = (page - 1) * size;
    return {
        items: items.slice(offset, offset + size),
        page,
        pages,
        total: items.length,
        start: items.length ? offset + 1 : 0,
        end: Math.min(offset + size, items.length),
    };
}

// Clamp a relative page change to the available page range
export function adjacentPage(page: { page: number; pages: number }, direction: number): number {
    return Math.min(page.pages, Math.max(1, page.page + direction));
}
// Update only the requested group page from the clicked control metadata
export function changeGroupPage(
    pages: Record<string, number>,
    data: DOMStringMap,
): Record<string, number> {
    return { ...pages, [data.key!]: Number(data.page) + Number(data.direction) };
}
