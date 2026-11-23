import type { Action, Binding, Manifest, Page, Profile } from './types';
import { positionedControlRect, previewGeometry } from './preview-layout';

function actionName(action: Action, profile: Profile): string {
    if (action.kind === 'plugin') return action.action_id;
    if (action.kind === 'page')
        return (
            'Go to ' + (profile.pages.find((p) => p.id === action.page_id)?.name || action.page_id)
        );
    if (action.kind === 'builtin') return action.action.kind.replaceAll('_', ' ');
    return action.kind.replaceAll('_', ' ');
}

function gestures(action: Action, profile: Profile): string {
    const lanes =
        action.kind === 'dial'
            ? ['Counterclockwise', 'Clockwise', 'Press']
            : action.kind === 'builtin' && action.action.kind === 'gesture'
              ? ['Tap', 'Double tap', 'Hold']
              : action.kind === 'builtin' && action.action.kind === 'press_release'
                ? ['Press', 'Release']
                : [];
    const children =
        action.kind === 'dial'
            ? action.actions
            : action.kind === 'builtin' && 'children' in action.action
              ? action.action.children
              : [];
    return lanes.length
        ? children
              .flatMap((a, i) =>
                  a.kind === 'none' ? [] : [lanes[i] + ' · ' + gestures(a, profile)],
              )
              .join(' / ')
        : actionName(action, profile);
}

export function staticLabel(b: Binding | undefined) {
    return (
        b?.title ||
        (b?.live_label || []).map((s) => (s.kind === 'literal' ? s.text : s.fallback)).join('')
    );
}

export function referenceLayout(profile: Profile, page: Page, model: Manifest) {
    const geometry = previewGeometry(
        model.controls,
        model.layout.width,
        model.model_id,
        model.capabilities.touch_panel,
    );
    return model.controls
        .filter((c) => !c.preview_hidden || !!page.bindings[c.id])
        .map((c, i) => {
            const b = page.bindings[c.id];
            const positioned = !c.preview_hidden && !c.semantics.includes('swipe');
            const rect = positionedControlRect(geometry, c);
            return {
                number: i + 1,
                control: c,
                binding: b,
                rect,
                positioned,
                legend: [
                    String(i + 1) + '. ' + c.label,
                    staticLabel(b),
                    b && b.action.kind !== 'none' ? gestures(b.action, profile) : '',
                ]
                    .filter(Boolean)
                    .join(' — '),
            };
        });
}
