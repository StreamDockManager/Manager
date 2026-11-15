import { test } from 'node:test';
import assert from 'node:assert/strict';
import { previewCadence } from '../src/preview-cadence.ts';

// Create isolated input for the preview cadence tests checks
function fixture() {
    let focused = true;
    let calls = 0;
    let timer;
    const events = new EventTarget();
    const cadence = previewCadence(
        () => calls++,
        () => focused,
        events,
        {
            // Replace the requested preview cadence tests value
            set(fn) {
                timer = fn;
                return 1;
            },
            // Clear the retained preview cadence tests value
            clear() {
                timer = undefined;
            },
        },
    );
    return {
        cadence,
        events,
        // Expose the captured cadence callback invocations for assertions
        calls: () => calls,
        // Expose the pending fake timer callback for deterministic execution
        timer: () => timer,
        // Blur the preview cadence tests operation
        blur() {
            focused = false;
        },
        // Focus the preview cadence tests operation
        focus() {
            focused = true;
            events.dispatchEvent(new Event('focus'));
        },
    };
}
// Verify focused subscriber acknowledges immediately; inactive subscriber holds one bounded update
test('focused subscriber acknowledges immediately; inactive subscriber holds one bounded update', () => {
    const f = fixture();
    f.cadence.ready();
    assert.equal(f.calls(), 1);
    f.blur();
    f.cadence.ready();
    const first = f.timer();
    f.cadence.ready();
    assert.equal(f.timer(), first);
    assert.equal(f.calls(), 1);
    first();
    assert.equal(f.calls(), 2);
    assert.equal(f.timer(), undefined);
    f.cadence.dispose();
});
// Verify focus resumes immediately and disposal cancels delayed acknowledgement
test('focus resumes immediately and disposal cancels delayed acknowledgement', () => {
    const f = fixture();
    f.blur();
    f.cadence.ready();
    f.focus();
    assert.equal(f.calls(), 1);
    assert.equal(f.timer(), undefined);
    f.blur();
    f.cadence.ready();
    const stale = f.timer();
    f.cadence.dispose();
    stale();
    f.focus();
    f.cadence.ready();
    assert.equal(f.calls(), 1);
});
