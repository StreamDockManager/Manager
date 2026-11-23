interface PreviewExpression {
    id: string;
    pattern: string;
    sensitive: boolean;
}
self.onmessage = ({
    data,
}: MessageEvent<{ conditions: PreviewExpression[]; title: string | null }>) => {
    try {
        const matches = Object.create(null) as Record<string, boolean>;
        for (const { id, pattern, sensitive } of data.conditions) {
            if (/\\[1-9]|\\k<|\(\?(?:[=!]|<[=!])/.test(pattern)) throw Error();
            const expression = new RegExp(pattern, sensitive ? 'u' : 'iu');
            matches[id] = data.title !== null && expression.test(data.title);
        }
        self.postMessage({ matches });
    } catch {
        self.postMessage({
            error: 'This expression is invalid or requires the desktop rule engine.',
        });
    }
};
export {};
