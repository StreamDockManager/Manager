import type { Manifest } from './types';
import type { PluginProfileSuggestion } from '../generated/runtime';

export interface ProfileSuggestionChoice extends PluginProfileSuggestion {
    source_key: string;
    family_id: string;
    label: string;
}

// Resolve a model's compatible profile family with its model ID as fallback
export function profileFamily(models: Manifest[], modelId: string) {
    return models.find((model) => model.model_id === modelId)?.profile_family || modelId;
}

/** One standard choice per hardware family, with the exact variant retained for import. */
export function profileSuggestions(
    suggestions: PluginProfileSuggestion[],
    models: Manifest[],
    preferredModel: string,
): ProfileSuggestionChoice[] {
    // Resolve a suggestion's model from its metadata or failed profile-file key
    const sourceModel = (item: PluginProfileSuggestion) =>
        item.model_id ||
        (item.error
            ? models.find((model) => item.key === `profiles/${model.model_id}.json`)?.model_id
            : '') ||
        '';
    const groups = new Map<string, PluginProfileSuggestion[]>();
    for (const suggestion of suggestions) {
        const model = sourceModel(suggestion);
        const standard = suggestion.key === `profiles/${model}.json`;
        const family = profileFamily(models, model);
        // Additional package-authored layouts remain independent choices
        const key = standard ? `profiles/${family}.json` : suggestion.key;
        const group = groups.get(key) || [];
        group.push(suggestion);
        groups.set(key, group);
    }
    // Convert each entry into the representation required by profile suggestions
    return [...groups].map(([key, variants]) => {
        const family = profileFamily(models, sourceModel(variants[0]));
        const chosen =
            variants.find((item) => !item.error && item.model_id === preferredModel) ||
            variants.find((item) => !item.error && item.model_id === family) ||
            variants.find((item) => !item.error) ||
            variants[0];
        const standard = key === `profiles/${family}.json`;
        const familyName = models.find((model) => model.model_id === family)?.name || family;
        return {
            ...chosen,
            key,
            source_key: chosen.key,
            family_id: family,
            label: chosen.error
                ? `${standard ? familyName : chosen.name} (unavailable)`
                : standard
                  ? familyName
                  : `${chosen.name} · ${chosen.model_id}`,
        };
    });
}
