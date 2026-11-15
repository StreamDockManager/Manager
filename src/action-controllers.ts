import Alpine from '@alpinejs/csp';
import { native } from './bridge';
import { app } from './route-context';
import { registerMacroEditor } from './macro-editor';
import { registerSavedActions, setKeyboardMacroNewActionRepeatMode } from './builtin-actions';

// Register action editors and load runtime capabilities
export async function register() {
    // The route loader awaits this before mounting either action editor, so the
    // first newly selected macro gets its native creation preference too
    setKeyboardMacroNewActionRepeatMode('exact');
    if (native) {
        try {
            const capabilities = await app().command('keyboard_macro_capabilities');
            setKeyboardMacroNewActionRepeatMode(capabilities.new_action_repeat_mode);
        } catch (error) {
            app().notify(String(error), 'error');
        }
    }
    registerMacroEditor(Alpine);
    registerSavedActions(Alpine);
    if (native)
        void app()
            .command('builtin_capabilities')
            // Expose discovered runtime capabilities to action editors
            .then((capabilities) => {
                app().builtinCapabilities = capabilities;
            })
            // Leave unavailable capabilities at their initial defaults
            .catch(() => {});
}
