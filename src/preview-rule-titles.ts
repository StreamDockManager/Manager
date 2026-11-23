import type { RuleTest } from '../generated/runtime';

// Keep hypothetical expressions off the UI thread and discard the worker after each test.
export async function previewRegexMatches(request: RuleTest): Promise<Record<string, boolean>> {
    const conditions = request.rules.flatMap(({ id, title }) =>
        title?.mode === 'regex'
            ? [{ id, pattern: title.value, sensitive: title.case_sensitive }]
            : [],
    );
    if (!conditions.length) return {};
    if (
        request.rules.length > 100 ||
        conditions.some((c) => c.pattern.length > 4096) ||
        (request.title?.length || 0) > 4096
    )
        throw Error('The rule test exceeds the preview limits.');
    const worker = new Worker(new URL('./preview-rule-worker.ts', import.meta.url), {
        type: 'module',
    });
    try {
        return await new Promise<Record<string, boolean>>((resolve, reject) => {
            const timer = setTimeout(
                () => reject(Error('The expression exceeded the browser preview time limit.')),
                1000,
            );
            worker.onmessage = ({ data }) => {
                clearTimeout(timer);
                if (data.error) reject(Error(data.error));
                else resolve(data.matches);
            };
            worker.onerror = () => {
                clearTimeout(timer);
                reject(Error('The preview rule worker is unavailable.'));
            };
            worker.postMessage({ conditions, title: request.title });
        });
    } finally {
        worker.terminate();
    }
}
