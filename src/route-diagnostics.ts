import { jumpToSection } from './visibility';
import Alpine from '@alpinejs/csp';
import { registerBenchmark } from './benchmark';
import { registerDependencies } from './dependencies';
import { createDiagnostics } from './diagnostics';
import { app } from './route-context';
// Register the diagnostics handler with its owner
export function register() {
    registerBenchmark();
    registerDependencies(Alpine, app);
    Alpine.data('diagnostics', () => Object.assign(createDiagnostics(app), { jumpToSection }));
}
