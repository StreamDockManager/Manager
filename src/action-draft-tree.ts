import type { Action } from './types';
import { cloneJson } from './json-clone';
import { InspectorActionDraft } from './inspector-action-draft';
import { oppositeTurnAction } from './dial-actions';

export type ActionNode = Pick<InspectorActionDraft, keyof InspectorActionDraft>;
export interface ActionDraftRecovery {
    actionDraft: ActionNode;
    dialActions: ActionNode[] | null;
    dialActionIndex: number;
    dialLinkedTurns: boolean;
}

/** One owner for each editable action and its unfinished text */
export class ActionDraftTree {
    single: ActionNode;
    currentLanes: ActionNode[] | null = null;
    lane = 2;
    linkedTurns = false;

    // Initialize editable action lanes from the supplied action
    constructor(action: Action = { kind: 'none' }) {
        this.single = new InspectorActionDraft(action);
        if (action.kind === 'dial') {
            if (action.actions.length !== 3)
                throw Error('A dial needs left, right and press actions.');
            this.currentLanes = action.actions.map((lane) => new InspectorActionDraft(lane));
            // New empty turns share one action; an explicit separate choice stays separate
            // Plugin contexts can update independently after saving. Do not discard
            // a newer right-side value merely by opening and saving appearance
            this.linkedTurns =
                (action.linked_turns ??
                    action.actions.slice(0, 2).every((lane) => lane.kind === 'none')) &&
                JSON.stringify(action.actions[1]) ===
                    JSON.stringify(oppositeTurnAction(action.actions[0]!));
            this.single = new InspectorActionDraft();
        }
    }
    // Read the currently selected action lane
    get active(): ActionNode {
        return (
            this.currentLanes?.[this.linkedTurns && this.lane === 1 ? 0 : this.lane] || this.single
        );
    }
    // Replace the currently selected action lane
    set active(draft: ActionNode) {
        if (this.currentLanes)
            this.currentLanes[this.linkedTurns && this.lane === 1 ? 0 : this.lane] = draft;
        else this.single = draft;
    }
    // Synchronize dial turn lanes when linked editing changes
    setLinkedTurns(linked: boolean) {
        if (!this.currentLanes) return;
        this.currentLanes.forEach((draft) => draft.commit());
        if (!linked && this.linkedTurns)
            this.currentLanes[1] = new InspectorActionDraft(
                oppositeTurnAction(this.currentLanes[0]!.commit()),
            );
        this.linkedTurns = linked;
        if (linked && this.lane === 1) this.lane = 0;
    }
    // Clear the requested dial action lane
    clearLane(index: number) {
        if (!this.currentLanes || ![0, 1, 2].includes(index)) return;
        if (index !== 2 && this.linkedTurns) {
            this.currentLanes[1] = new InspectorActionDraft(
                oppositeTurnAction(this.currentLanes[0]!.action),
            );
            this.linkedTurns = false;
        }
        this.currentLanes[index] = new InspectorActionDraft();
    }
    // Project draft lanes into an action for commit or preview
    private action(committed: boolean): Action {
        // Validate committed lanes while cloning uncommitted previews
        const read = (draft: ActionNode) => (committed ? draft.commit() : cloneJson(draft.action));
        if (!this.currentLanes) return read(this.single);
        const left = read(this.currentLanes[0]!);
        const actions = [
            left,
            this.linkedTurns ? oppositeTurnAction(left) : read(this.currentLanes[1]!),
            read(this.currentLanes[2]!),
        ];
        return { kind: 'dial', actions, linked_turns: this.linkedTurns };
    }
    // Commit the edited action tree after validation
    commit() {
        return this.action(true);
    }
    // Project the current tree without committing draft edits
    preview() {
        return this.action(false);
    }
    // Capture action lanes and selection for draft recovery
    recovery(active: ActionNode = this.active): ActionDraftRecovery {
        const lane = this.linkedTurns && this.lane === 1 ? 0 : this.lane;
        return {
            actionDraft: active,
            dialActions:
                this.currentLanes?.map((draft, index) => (index === lane ? active : draft)) || null,
            dialActionIndex: lane,
            dialLinkedTurns: this.linkedTurns,
        };
    }
    // Restore a saved action-tree draft after validating its shape
    static restore(value: Record<string, unknown>) {
        const active = InspectorActionDraft.restore(value);
        const lanes = value.dialActions;
        const lane = value.dialActionIndex ?? 2;
        if (![0, 1, 2].includes(lane as number)) throw Error('Invalid dial action selection.');
        const tree = new ActionDraftTree();
        if (lanes != null) {
            if (!Array.isArray(lanes) || lanes.length !== 3)
                throw Error('A dial needs left, right and press actions.');
            tree.currentLanes = lanes.map((actionDraft) =>
                InspectorActionDraft.restore({ actionDraft }),
            );
        }
        if (value.dialLinkedTurns !== undefined && typeof value.dialLinkedTurns !== 'boolean')
            throw Error('Invalid linked turn mode.');
        tree.linkedTurns = value.dialLinkedTurns === true;
        if (tree.linkedTurns && !tree.currentLanes) throw Error('Linked turns require a dial.');
        tree.lane = tree.linkedTurns && lane === 1 ? 0 : (lane as number);
        tree.active = active;
        return tree;
    }
}
