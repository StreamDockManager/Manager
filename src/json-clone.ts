/** Clone JSON-shaped state, normalizing reactive proxies via serialization.
 * Undefined roots remain undefined; nested undefined follows JSON semantics */
export function cloneJson<T>(value: T): T {
    return value === undefined ? value : JSON.parse(JSON.stringify(value));
}
