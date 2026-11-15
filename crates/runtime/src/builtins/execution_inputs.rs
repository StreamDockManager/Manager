use super::{Runtime, SavedMacro, State};
use std::sync::Arc;

/// A debugger owns only its draft overlay. The saved revision remains shared
/// with ordinary activations and other debugger sessions
#[derive(Clone, Default)]
pub(super) struct MacroLibrary {
    pub(super) saved: Arc<[SavedMacro]>,
    draft: Option<Arc<SavedMacro>>,
}

impl From<Vec<SavedMacro>> for MacroLibrary {
    // Convert the input into the execution inputs representation
    fn from(macros: Vec<SavedMacro>) -> Self {
        Self {
            saved: macros.into(),
            draft: None,
        }
    }
}

impl MacroLibrary {
    // Iterate saved macros with the current draft replacing any matching saved definition
    pub(super) fn iter(&self) -> impl Iterator<Item = &SavedMacro> {
        self.saved
            .iter()
            .filter(|saved| self.draft.as_ref().is_none_or(|draft| draft.id != saved.id))
            .chain(self.draft.as_deref())
    }

    // Construct using draft
    pub(super) fn with_draft(&self, draft: SavedMacro) -> Self {
        Self {
            saved: self.saved.clone(),
            draft: Some(Arc::new(draft)),
        }
    }
}

#[derive(Clone)]
pub(super) struct ExecutionInputs {
    revision: u64,
    pub(super) macros: MacroLibrary,
    pub(super) profiles: Arc<[crate::Profile]>,
}

impl State {
    // Reuse or rebuild immutable execution inputs for the current configuration revision
    pub(super) fn execution_inputs(&self, runtime: &Runtime) -> ExecutionInputs {
        // Lock order is runtime state -> this cache. No path holds the cache
        // while acquiring runtime state or doing persistence/host operations
        let state = runtime.state.lock().unwrap();
        let mut cached = self.execution_inputs.lock().unwrap();
        if let Some(inputs) = &*cached {
            if inputs.revision == state.configuration_revision {
                return inputs.clone();
            }
        }
        let inputs = ExecutionInputs {
            revision: state.configuration_revision,
            macros: state.saved_macros.clone().into(),
            profiles: state.profiles.clone().into(),
        };
        *cached = Some(inputs.clone());
        inputs
    }
}

impl ExecutionInputs {
    // Resolve execution parameters before the action begins
    pub(super) fn freeze_parameters(
        &self,
        runtime: &Runtime,
    ) -> crate::parameters::FrozenParameters {
        // Local overrides can change without a workspace revision. Sample per activation
        crate::parameters::FrozenParameters::new(
            self.profiles.clone(),
            runtime.stored_parameter_overrides(),
        )
    }
}
