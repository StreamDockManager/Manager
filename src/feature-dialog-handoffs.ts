/** Only cross-workflow requests live here, never a dialog's draft or busy/error state */
export class FeatureDialogHandoffs {
    private finishSetup!: () => void;
    readonly setupReady = new Promise<void>((resolve) => {
        this.finishSetup = resolve;
    });

    // Finish the pending setup handoff after initialization
    setupInitialized() {
        this.finishSetup();
    }

    // Default to no active setup dialog until a controller connects
    private isSetupOpen = () => false;
    // Provide an idle suggestion handoff until a controller connects
    private resume = () => {};
    private review: ((id: string, key: string, deviceId?: string) => Promise<void>) | undefined;

    // Attach the setup-state reader and return an identity-safe disconnect handler
    connectSetup(isOpen: () => boolean) {
        this.isSetupOpen = isOpen;
        return () => {
            if (this.isSetupOpen === isOpen) this.isSetupOpen = () => false;
        };
    }
    // Attach the suggestion-resume callback until its owner disconnects
    connectSuggestions(resume: () => void) {
        this.resume = resume;
        return () => {
            if (this.resume === resume) this.resume = () => {};
        };
    }
    // Attach the profile-import reviewer until its owner disconnects
    connectImport(review: (id: string, key: string, deviceId?: string) => Promise<void>) {
        this.review = review;
        return () => {
            if (this.review === review) this.review = undefined;
        };
    }
    // Read whether the connected setup dialog is open
    setupOpen() {
        return this.isSetupOpen();
    }
    // Resume suggestions through the currently connected controller
    resumeSuggestions() {
        this.resume();
    }
    // Forward a suggested profile to the connected import reviewer
    async importSuggestion(id: string, key: string, deviceId?: string) {
        if (!this.review) throw Error('Profile import is not ready. Please try again.');
        await this.review(id, key, deviceId);
    }
}
