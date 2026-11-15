import { actionLibrarySelection, type ActionLibraryRequest } from './action-library-selection';
// Open the shared action-library selection flow
export function openActionPicker(request: ActionLibraryRequest) {
    actionLibrarySelection.open(request);
}
