export interface JsonHighlight {
    start: number;
    end: number;
    message?: string;
    anchor?: boolean;
}
// Find json matches for the recovery json editor workflow
export function findJsonMatches(text: string, query: string): JsonHighlight[] {
    if (!query) return [];
    const matches: JsonHighlight[] = [];
    let offset = 0;
    while (matches.length < 1000) {
        const start = text.indexOf(query, offset);
        if (start < 0) break;
        matches.push({ start, end: start + query.length });
        offset = start + query.length;
    }
    return matches;
}
export class RecoveryJsonEditor {
    private matches: JsonHighlight[] = [];
    private issues: JsonHighlight[] = [];
    private current = -1;
    private value = '';
    private timer: ReturnType<typeof setTimeout> | undefined;
    // Initialize recovery json editor state and its required lifecycle hooks
    constructor(
        private input: HTMLTextAreaElement,
        private layer: HTMLElement,
        private search: HTMLInputElement,
        private count: HTMLElement,
        private previous: HTMLButtonElement,
        private next: HTMLButtonElement,
    ) {
        input.addEventListener('scroll', () => this.syncScroll());
        // Respond to input events in the recovery json editor workflow
        search.addEventListener('input', () => {
            this.current = -1;
            clearTimeout(this.timer);
            this.timer = setTimeout(() => this.updateMatches(), 120);
        });
        // Respond to keydown events in the recovery json editor workflow
        search.addEventListener('keydown', (event) => {
            if (event.key === 'Enter') {
                event.preventDefault();
                this.move(event.shiftKey ? -1 : 1);
            }
            if (event.key === 'Escape') {
                event.preventDefault();
                event.stopPropagation();
                search.value = '';
                this.updateMatches();
                input.focus();
            }
        });
        previous.addEventListener('click', () => this.move(-1));
        next.addEventListener('click', () => this.move(1));
    }
    // Focus the search
    focusSearch() {
        this.search.focus();
        this.search.select();
    }
    // Update the current recovery json editor state
    update(value: string, issues: JsonHighlight[]) {
        if (this.value === value && JSON.stringify(this.issues) === JSON.stringify(issues)) return;
        const changed = this.value !== value;
        this.value = value;
        this.issues = issues.filter(
            (issue) =>
                Number.isInteger(issue.start) &&
                Number.isInteger(issue.end) &&
                issue.start >= 0 &&
                issue.end > issue.start &&
                issue.end <= value.length,
        );
        clearTimeout(this.timer);
        if (changed) {
            this.current = -1;
            this.matches = [];
            this.layer.replaceChildren();
            this.previous.disabled = this.next.disabled = true;
            this.timer = setTimeout(() => this.updateMatches(), 120);
        } else this.updateMatches();
    }
    // Update the matches
    private updateMatches() {
        this.timer = undefined;
        this.matches = findJsonMatches(this.value, this.search.value);
        this.count.textContent = this.search.value
            ? `${this.current < 0 ? 0 : this.current + 1} / ${this.matches.length}${this.matches.length === 1000 ? ' (first 1,000)' : ''}`
            : '';
        this.previous.disabled = this.next.disabled = this.matches.length === 0;
        this.paint();
    }
    // Move the selected recovery json editor item
    private move(direction: number) {
        if (this.timer !== undefined) {
            clearTimeout(this.timer);
            this.updateMatches();
        }
        if (!this.matches.length) return;
        this.current =
            this.current < 0
                ? direction < 0
                    ? this.matches.length - 1
                    : 0
                : (this.current + direction + this.matches.length) % this.matches.length;
        this.reveal(this.matches[this.current], false);
        this.count.textContent = `${this.current + 1} / ${this.matches.length}${this.matches.length === 1000 ? ' (first 1,000)' : ''}`;
        const selected = this.matches[this.current];
        // Apply the recovery json editor update to each selected entry
        this.layer.querySelectorAll<HTMLElement>('mark').forEach((mark) => {
            mark.classList.toggle(
                'json-current',
                Number(mark.dataset.start) >= selected.start &&
                    Number(mark.dataset.end) <= selected.end,
            );
        });
    }
    // Reveal the selected recovery json editor item
    reveal(range: JsonHighlight, focus = true) {
        if (focus) this.input.focus();
        this.input.setSelectionRange(range.start, range.end);
        const preceding = this.value.slice(0, range.start);
        const lines = preceding.split('\n');
        const style = getComputedStyle(this.input);
        this.input.scrollTop = Math.max(
            0,
            (lines.length - 1) * parseFloat(style.lineHeight) - this.input.clientHeight / 3,
        );
        const measure = document.createElement('span');
        measure.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;tab-size:2';
        measure.style.font = style.font;
        measure.textContent = lines.at(-1)!;
        this.layer.parentElement!.append(measure);
        const width = measure.getBoundingClientRect().width;
        measure.remove();
        this.input.scrollLeft = Math.max(0, width - this.input.clientWidth / 3);
        this.syncScroll();
    }
    // Sync the scroll
    private syncScroll() {
        this.layer.style.transform = `translate(${-this.input.scrollLeft}px, ${-this.input.scrollTop}px)`;
    }
    // Paint the current recovery json editor frame
    private paint() {
        const ranges = [
            ...this.issues.map((range) => ({ ...range, kind: 'invalid' })),
            ...this.matches.map((range) => ({ ...range, kind: 'match' })),
            ...(this.current >= 0 && this.matches[this.current]
                ? [{ ...this.matches[this.current], kind: 'current' }]
                : []),
        ];
        const events = ranges
            .flatMap((range) => [
                { at: range.start, kind: range.kind, delta: 1 },
                { at: range.end, kind: range.kind, delta: -1 },
            ])
            .sort((a, b) => a.at - b.at);
        const fragment = document.createDocumentFragment();
        const active = new Map<string, number>();
        let offset = 0;
        // Append the new entry to the recovery json editor collection
        const append = (end: number) => {
            if (end <= offset) return;
            const kinds = [...active].filter(([, count]) => count > 0).map(([kind]) => kind);
            const content = this.value.slice(offset, end);
            if (kinds.length) {
                const mark = document.createElement('mark');
                mark.className = kinds.map((kind) => `json-${kind}`).join(' ');
                mark.textContent = content;
                mark.dataset.start = String(offset);
                mark.dataset.end = String(end);
                fragment.append(mark);
            } else fragment.append(document.createTextNode(content));
            offset = end;
        };
        for (const event of events) {
            append(event.at);
            active.set(event.kind, (active.get(event.kind) || 0) + event.delta);
        }
        append(this.value.length);
        fragment.append(document.createTextNode('\n'));
        this.layer.replaceChildren(fragment);
        this.syncScroll();
    }
}
