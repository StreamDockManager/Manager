// Choose a scalar default matching the parameter's declared kind
export function initialValue(kind: string): string | number | boolean {
    return kind === 'number' ? 0 : kind === 'boolean' ? false : '';
}
// Parse the value
export function parseValue(kind: string, value: string | boolean): string | number | boolean {
    if (kind === 'number') {
        if (typeof value !== 'string' || !value.trim() || !Number.isFinite(Number(value)))
            throw Error('Enter a finite number.');
        return Number(value);
    }
    if (kind === 'boolean') {
        if (typeof value === 'boolean') return value;
        if (value !== 'true' && value !== 'false') throw Error('Choose true or false.');
        return value === 'true';
    }
    return String(value);
}
