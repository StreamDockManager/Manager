const SEGMENT = 8192;
const MAX_FRAMES = 4_000_000;

/** A recording index stays segmented while sorting, deduplicating and seeking. */
export class VideoIndex {
    private segments: Float64Array[] = [];
    private ordered = true;
    length = 0;

    // Derive allocated bytes from the current video index state
    get allocatedBytes() {
        return this.segments.length * SEGMENT * Float64Array.BYTES_PER_ELEMENT;
    }
    // Read the video index item at the requested position
    at(index: number): number | undefined {
        return index >= 0 && index < this.length ? this.get(index) : undefined;
    }
    // Read the requested video index value
    private get(index: number) {
        return this.segments[index >>> 13][index & (SEGMENT - 1)];
    }
    // Replace the requested video index value
    private set(index: number, value: number) {
        this.segments[index >>> 13][index & (SEGMENT - 1)] = value;
    }
    // Append the new entry to the video index collection
    append(value: number) {
        if (!Number.isFinite(value)) throw Error('Invalid recording timestamp');
        if (this.length >= MAX_FRAMES) throw Error('Recording frame index exceeds limits');
        if (this.length && value < this.get(this.length - 1)) this.ordered = false;
        if (this.length % SEGMENT === 0) this.segments.push(new Float64Array(SEGMENT));
        this.set(this.length++, value);
    }
    // Exchange two values in the indexed timestamp storage
    private swap(a: number, b: number) {
        const value = this.get(a);
        this.set(a, this.get(b));
        this.set(b, value);
    }
    // Sort the inclusive timestamp range in place using heapsort
    private heapSort(first: number, last: number) {
        // Restore the max-heap property by moving a parent down toward its larger child
        const sift = (root: number, length: number) => {
            while (root * 2 + 1 < length) {
                let child = root * 2 + 1;
                if (child + 1 < length && this.get(first + child) < this.get(first + child + 1))
                    child++;
                if (this.get(first + root) >= this.get(first + child)) return;
                this.swap(first + root, first + child);
                root = child;
            }
        };
        const length = last - first + 1;
        for (let root = Math.floor(length / 2) - 1; root >= 0; root--) sift(root, length);
        for (let end = length - 1; end > 0; end--) {
            this.swap(first, first + end);
            sift(0, end);
        }
    }
    // Sort the video index operation
    private sort(first: number, last: number, depth: number) {
        while (last - first > 24) {
            if (depth-- === 0) {
                this.heapSort(first, last);
                return;
            }
            const a = this.get(first),
                b = this.get((first + last) >>> 1),
                c = this.get(last);
            const pivot = Math.max(Math.min(a, b), Math.min(Math.max(a, b), c));
            let left = first,
                right = last;
            while (left <= right) {
                while (this.get(left) < pivot) left++;
                while (this.get(right) > pivot) right--;
                if (left <= right) this.swap(left++, right--);
            }
            // Recurse only into the smaller partition, bounding the call stack
            if (right - first < last - left) {
                this.sort(first, right, depth);
                first = left;
            } else {
                this.sort(left, last, depth);
                last = right;
            }
        }
        for (let index = first + 1; index <= last; index++) {
            const value = this.get(index);
            let target = index;
            while (target > first && this.get(target - 1) > value) {
                this.set(target, this.get(target - 1));
                target--;
            }
            this.set(target, value);
        }
    }
    // Finish the current video index operation and retire its pending state
    finish() {
        if (!this.ordered) this.sort(0, this.length - 1, 2 * Math.floor(Math.log2(this.length)));
        let unique = 0;
        for (let index = 0; index < this.length; index++) {
            const time = this.get(index);
            if (unique === 0 || time - this.get(unique - 1) >= 0.000001) this.set(unique++, time);
        }
        this.length = unique;
        this.segments.length = Math.ceil(unique / SEGMENT);
        this.ordered = true;
    }
    // Find the first timestamp not less than the requested value with binary search
    lowerBound(value: number) {
        let left = 0,
            right = this.length;
        while (left < right) {
            const middle = (left + right) >>> 1;
            if (this.get(middle) < value) left = middle + 1;
            else right = middle;
        }
        return left;
    }
    // Copy a half-open timestamp range while clamping each value to the minimum
    range(first: number, last: number, minimum: number) {
        return Array.from({ length: last - first }, (_, index) =>
            Math.max(this.get(first + index), minimum),
        );
    }
    // Infer source frame rate and variable timing from positive timestamp gaps
    timing(duration: number) {
        let smallest = Infinity,
            largest = 0;
        for (let index = 1; index < this.length; index++) {
            const delay = this.get(index) - this.get(index - 1);
            if (delay > 0) {
                smallest = Math.min(smallest, delay);
                largest = Math.max(largest, delay);
            }
        }
        return {
            source_fps: Number.isFinite(smallest) ? 1 / smallest : 1 / duration,
            variable_rate: largest - smallest > 0.0001,
        };
    }
}
