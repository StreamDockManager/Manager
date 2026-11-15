import type { JsonHighlight } from './recovery-json-editor';
export interface RecoveryDiagnostic {
    message: string;
    pointer: string | null;
    line: number | null;
    column: number | null;
    anchor?: boolean;
}
/** Resolve pointers structurally, never by searching repeated property names. */
export function locateDiagnostics(
    json: string,
    diagnostics: RecoveryDiagnostic[],
): (JsonHighlight | null)[] {
    const wanted = new Set(diagnostics.flatMap((d) => (d.pointer === null ? [] : [d.pointer])));
    const located = new Map<string, JsonHighlight>();
    let at = 0;
    // Advance the parser cursor past JSON whitespace
    const space = () => {
        while (/\s/.test(json[at] || '') && at < json.length) at++;
    };
    // Parse a quoted JSON string while skipping escaped characters
    const string = () => {
        const start = at++;
        while (at < json.length) {
            const c = json[at++];
            if (c === '\\') at++;
            else if (c === '"') return JSON.parse(json.slice(start, at)) as string;
        }
        throw Error('Incomplete string');
    };
    // Return the current recovery json locations value
    const value = (pointer: string, depth: number) => {
        if (depth > 256) throw Error('Deep JSON');
        space();
        const start = at;
        if (json[at] === '{') {
            at++;
            space();
            const keys = new Set<string>();
            while (json[at] !== '}') {
                if (json[at] !== '"') throw Error('Invalid object');
                const keyStart = at,
                    key = string(),
                    keyEnd = at;
                if (keys.has(key)) throw Error('Ambiguous duplicate key');
                keys.add(key);
                const child = `${pointer}/${key.replace(/~/g, '~0').replace(/\//g, '~1')}`;
                space();
                if (json[at++] !== ':') throw Error('Missing colon');
                value(child, depth + 1);
                if (wanted.has(child)) located.set(child, { start: keyStart, end: keyEnd });
                space();
                if (json[at] !== ',') break;
                at++;
                space();
            }
            if (json[at++] !== '}') throw Error('Incomplete object');
        } else if (json[at] === '[') {
            at++;
            space();
            let index = 0;
            while (json[at] !== ']') {
                value(`${pointer}/${index++}`, depth + 1);
                space();
                if (json[at] !== ',') break;
                at++;
            }
            if (json[at++] !== ']') throw Error('Incomplete array');
        } else if (json[at] === '"') string();
        else {
            while (at < json.length && !/[\s,}\]]/.test(json[at])) at++;
            if (at === start) throw Error('Missing value');
        }
        if (wanted.has(pointer) && !located.has(pointer)) located.set(pointer, { start, end: at });
    };
    if (wanted.size) {
        try {
            JSON.parse(json);
            value('', 0);
            space();
            if (at !== json.length) throw Error('Trailing JSON');
        } catch {
            located.clear(); /* syntax location still available */
        }
    }
    const encoder = new TextEncoder();
    // Convert each entry into the representation required by recovery json locations
    return diagnostics.map((diagnostic) => {
        const exact = diagnostic.pointer === null ? undefined : located.get(diagnostic.pointer);
        if (exact) return { ...exact, message: diagnostic.message, anchor: diagnostic.anchor };
        if (
            diagnostic.line === null ||
            diagnostic.column === null ||
            diagnostic.line < 1 ||
            diagnostic.column < 1
        )
            return null;
        let start = 0;
        for (let line = 1; line < diagnostic.line; line++) {
            const end = json.indexOf('\n', start);
            if (end < 0) return null;
            start = end + 1;
        }
        let bytes = 0;
        // serde's column identifies the last consumed UTF-8 byte (1-based)
        for (const character of json.slice(start)) {
            if (character === '\n') break;
            const size = encoder.encode(character).length;
            if (bytes + size >= diagnostic.column)
                return {
                    start,
                    end: start + character.length,
                    message: diagnostic.message,
                    anchor: diagnostic.anchor,
                };
            bytes += size;
            start += character.length;
        }
        if (start > 0)
            return {
                start:
                    start > 1 &&
                    /[\uDC00-\uDFFF]/.test(json[start - 1]) &&
                    /[\uD800-\uDBFF]/.test(json[start - 2])
                        ? start - 2
                        : start - 1,
                end: start,
                message: diagnostic.message,
                anchor: true,
            };
        return null;
    });
}
