import type { UpdatePreferences } from '../generated/runtime';

// Resolve an implicit source without discarding an older explicitly configured CDN URL
export function catalogSource(saved: UpdatePreferences) {
    return (
        saved.plugins_catalog_source ||
        (saved.plugins_catalog_url?.trim() ? 'hosted' : 'repository')
    );
}

/** Application and catalog drafts deliberately own disjoint saved preference fields. */
export class ApplicationUpdateDraft {
    repository = '';
    automatic = false;
    // Load the data required by the update preference drafts workflow
    load(saved: UpdatePreferences) {
        this.repository = saved.manager_repository;
        this.automatic = saved.automatic_checks;
    }
    // Detect unsaved Manager repository or automatic-check changes
    dirty(saved: UpdatePreferences) {
        return (
            this.repository !== saved.manager_repository ||
            this.automatic !== saved.automatic_checks
        );
    }
}
export class CatalogUpdateDraft {
    repository = '';
    url = '';
    source = 'repository';
    channel = 'stable';
    // Load the data required by the update preference drafts workflow
    load(saved: UpdatePreferences) {
        this.repository = saved.plugins_repository;
        this.url = saved.plugins_catalog_url || '';
        this.source = catalogSource(saved);
        this.channel = saved.plugin_channel;
    }
    // Detect unsaved plugin catalog source, repository, URL or channel changes
    dirty(saved: UpdatePreferences) {
        return (
            this.repository !== saved.plugins_repository ||
            this.url !== (saved.plugins_catalog_url || '') ||
            this.source !== catalogSource(saved) ||
            this.channel !== saved.plugin_channel
        );
    }
}
