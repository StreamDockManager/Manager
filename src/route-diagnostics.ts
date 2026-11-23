import { pageSections } from './tab-navigation';
import Alpine from '@alpinejs/csp';
import { registerBenchmark } from './benchmark';
import { registerDependencies } from './dependencies';
import { createDiagnostics } from './diagnostics';
import { app } from './route-context';
// Register the diagnostics handler with its owner
export function register() {
    registerBenchmark();
    registerDependencies(Alpine, app);
    Alpine.data('diagnostics', () =>
        Object.assign(
            createDiagnostics(app),
            pageSections('diagnostics', [
                { id: 'resources', label: 'Resources' },
                { id: 'benchmarks', label: 'Benchmarks' },
                { id: 'trace', label: 'Trace' },
                { id: 'dependencies', label: 'Dependencies' },
                { id: 'events', label: 'Events' },
            ]),
        ),
    );
}
