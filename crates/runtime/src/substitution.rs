//! Mechanical action substitution. Each caller retains its resolution and validation policy
use crate::actions::Action;
use serde_json::Value;

// Clone an action and substitute supported text fields and plugin settings through caller-provided resolvers
pub(crate) fn action(
    source: &Action,
    text: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    settings: &mut impl FnMut(&str, &str, &mut Value) -> Result<(), String>,
) -> Result<Action, String> {
    let mut action = source.clone();
    action.try_visit_fields_mut(
        &mut |role, value| {
            if role.supports_parameter() {
                if let Some(replacement) = text(value)? {
                    *value = replacement;
                }
            }
            Ok(())
        },
        settings,
    )?;
    Ok(action)
}
