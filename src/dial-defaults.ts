import type { Action } from './types';

/** Apply lane defaults only to a newly chosen action, never to loaded or saved actions */
export function dialActionDefault(action: Action, lane: number | null): Action {
    if (lane !== 0 && lane !== 1) return action;
    if (action.kind === 'volume')
        return { ...action, delta: (lane === 0 ? -1 : 1) * Math.abs(action.delta) };
    if (action.kind === 'page' || action.kind === 'profile')
        return {
            kind: 'navigate',
            target: action.kind,
            direction: lane === 0 ? 'previous' : 'next',
        };
    if (action.kind === 'navigate')
        return { ...action, direction: lane === 0 ? 'previous' : 'next' };
    return action;
}
